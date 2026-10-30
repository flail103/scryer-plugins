//! One paged search invocation: the query to send, and how its answer
//! becomes one response page.
//!
//! The HTTP transport is a trait so the whole flow runs against fixtures in
//! native tests.

use unicode_normalization::UnicodeNormalization;

use scryer_plugin_sdk::{
    PluginSearchRequest as SearchRequest, PluginSearchResponse as SearchResponse,
};

use crate::api::{self, Entry, MAX_OFFSET, MAX_RESULTS, MvwConfig, MvwError, QueryCall};
use crate::cursor::PageCursor;
use crate::release::{build_result, variant};

/// Pages one search may read before it stops. The host keeps following the
/// cursor until it has as many results as it asked for; MediathekViewWeb is
/// a free community service, so the plugin bounds that here.
pub const PAGE_CEILING: u32 = 2;

pub struct HttpReply {
    pub status: u16,
    /// The `Retry-After` header, if the reply sent one.
    pub retry_after: Option<String>,
    pub body: Vec<u8>,
}

/// One `POST` of a query body to the API.
pub trait Fetch {
    async fn post(&self, call: &QueryCall) -> Result<HttpReply, MvwError>;
}

#[derive(Debug)]
pub struct PageOutcome {
    pub response: SearchResponse,
    /// The search stopped at [`PAGE_CEILING`] with more results unread.
    pub ceiling_reached: bool,
}

/// The text sent to MediathekViewWeb: the request's query with its
/// whitespace collapsed. Season and episode numbers mean nothing to the
/// film list, which titles programmes by broadcast date and needs every
/// query word to match. So they are not added, and the `S2026E41` or `S2026`
/// words the host puts in an episode query are dropped; the host matches
/// the returned air dates to its episodes.
pub fn build_query(request: &SearchRequest) -> String {
    request
        .query
        .split_whitespace()
        .filter(|word| !is_numbering_word(word))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `S<digits>`, optionally followed by `E<digits>`, in either case.
fn is_numbering_word(word: &str) -> bool {
    let Some(rest) = word.strip_prefix(['S', 's']) else {
        return false;
    };
    let (season, episode) = match rest.find(['E', 'e']) {
        Some(at) => (&rest[..at], Some(&rest[at + 1..])),
        None => (rest, None),
    };
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    digits(season) && episode.is_none_or(digits)
}

/// The host resolves IDs to titles; MVW itself never receives an ID.
pub fn has_external_id(request: &SearchRequest) -> bool {
    ["tvdb_id", "tmdb_id"].iter().any(|key| {
        request.ids.get(*key).is_some_and(|id| {
            let id = id.trim();
            !id.is_empty()
                && id.bytes().all(|b| b.is_ascii_digit())
                && id.bytes().any(|b| b != b'0')
        })
    })
}

pub const MAX_CANDIDATES: usize = 5;

pub fn words(text: &str) -> Vec<String> {
    let clean: String = text
        .nfc()
        .collect::<String>()
        .to_lowercase()
        .chars()
        .map(|ch| if ch.is_alphanumeric() { ch } else { ' ' })
        .collect();
    clean
        .split_whitespace()
        .filter(|word| !is_numbering_word(word))
        .filter(|word| !word.chars().all(|ch| ch.is_numeric()))
        .map(str::to_string)
        .collect()
}

fn distinct(words: &[String]) -> Vec<String> {
    let mut unique = Vec::new();
    for word in words {
        if word.chars().count() >= 2 && !unique.contains(word) {
            unique.push(word.clone());
        }
    }
    unique
}

pub fn search_titles(request: &SearchRequest) -> (Vec<String>, Vec<String>) {
    let series = request
        .context
        .as_ref()
        .and_then(|context| {
            context
                .scene_titles
                .iter()
                .find(|title| !distinct(&words(title)).is_empty())
        })
        .map(String::as_str)
        .or_else(|| {
            (!distinct(&words(&request.query)).is_empty()).then_some(request.query.as_str())
        })
        .or_else(|| {
            request
                .tagged_aliases
                .iter()
                .map(|alias| alias.name.as_str())
                .find(|title| !distinct(&words(title)).is_empty())
        })
        .unwrap_or("");
    let episode = request
        .context
        .as_ref()
        .and_then(|context| context.episode_title.as_deref())
        .unwrap_or("");
    (words(series), words(episode))
}

/// Document-frequency weights from the first (series) query, frozen for the
/// entire search so narrower replies cannot inflate their own confidence.
/// Weights range from 100 to 300. Unseen words receive only 150; fewer than
/// three documents give uniform weights because rarity is not meaningful yet.
struct Keyword {
    word: String,
    weight: usize,
}

pub struct Relevance {
    series: Vec<String>,
    episode: Vec<Keyword>,
    has_episode: bool,
}

impl Relevance {
    pub fn new(series: &[String], episode: &[String], sample: &[Entry]) -> Self {
        let series = distinct(series);
        let documents = sample
            .iter()
            .take(MAX_RESULTS)
            .map(|entry| words(&format!("{} {}", entry.topic, entry.title)))
            .collect::<Vec<_>>();
        let mut keys = distinct(episode)
            .into_iter()
            // A word already required by the series cannot establish episode identity.
            .filter(|word| !series.contains(word))
            .map(|word| {
                let frequency = documents.iter().filter(|doc| doc.contains(&word)).count();
                let weight = if documents.len() < 3 {
                    100
                } else if frequency == 0 {
                    150
                } else {
                    (100 * (documents.len() + 1) / (frequency + 1)).min(300)
                };
                Keyword { word, weight }
            })
            .collect::<Vec<_>>();
        // Stable sorting keeps source order for equal weights; length is not a proxy.
        keys.sort_by_key(|key| std::cmp::Reverse(key.weight));
        Self {
            series,
            episode: keys,
            has_episode: !episode.is_empty(),
        }
    }

    pub fn keywords(&self) -> Vec<String> {
        self.episode.iter().map(|key| key.word.clone()).collect()
    }

    pub fn score(&self, entry: &Entry) -> Option<usize> {
        let result = words(&format!("{} {}", entry.topic, entry.title));
        let series_hits = self
            .series
            .iter()
            .filter(|word| result.contains(word))
            .count();
        if !self.series.is_empty() && series_hits * 4 < self.series.len() * 3 {
            return None;
        }
        let hits = self
            .episode
            .iter()
            .filter(|key| result.contains(&key.word))
            .collect::<Vec<_>>();
        let matched: usize = hits.iter().map(|key| key.weight).sum();
        let total: usize = self.episode.iter().map(|key| key.weight).sum();
        if self.has_episode {
            // Require 70% weighted coverage and two independent words when
            // available. A one-word title also needs series evidence.
            if total == 0
                || matched * 10 < total * 7
                || hits.len() < self.episode.len().min(2)
                || (self.series.is_empty() && hits.len() < 2)
            {
                return None;
            }
        } else if self.series.is_empty() {
            return None;
        }
        Some(
            50 * usize::from(!self.series.is_empty())
                + (50 * matched).checked_div(total).unwrap_or(0),
        )
    }
}

/// The broad sample is followed by at most four distinct narrower queries.
/// The caller skips candidates equal to the sample query.
pub fn candidate_queries(series: &[String], episode: &[String], keys: &[String]) -> Vec<String> {
    let series = series.join(" ");
    let mut queries = Vec::new();
    let mut add = |query: String| {
        let query = query.trim().to_string();
        if !query.is_empty() && !queries.contains(&query) {
            queries.push(query);
        }
    };
    if !series.is_empty() && !episode.is_empty() {
        add(format!("{series} {}", episode.join(" ")));
        if !keys.is_empty() {
            add(format!(
                "{series} {}",
                keys.iter().take(3).cloned().collect::<Vec<_>>().join(" ")
            ));
            add(format!("{series} {}", keys[0]));
        }
    }
    add(episode.join(" "));
    queries.truncate(MAX_CANDIDATES - 1);
    queries
}

async fn run_fallback<F: Fetch>(
    config: &MvwConfig,
    request: &SearchRequest,
    fetch: &F,
) -> Result<PageOutcome, MvwError> {
    // Fallback is a bounded, terminal batch, never a paged fan-out.
    if request.page_cursor.is_some() {
        return Err(MvwError::InvalidCursor);
    }
    let (series, episode) = search_titles(request);
    let series = distinct(&series);
    let episode = distinct(&episode);
    let initial = if series.is_empty() {
        episode.join(" ")
    } else {
        series.join(" ")
    };
    if initial.is_empty() {
        return Ok(PageOutcome {
            response: SearchResponse::default(),
            ceiling_reached: false,
        });
    }
    let mut queries = vec![initial];
    let mut relevance = None;
    let mut index = 0;
    while index < queries.len() && index < MAX_CANDIDATES {
        let call = QueryCall {
            query: queries[index].clone(),
            channel: match config.channels.as_slice() {
                [only] => Some(only.clone()),
                _ => None,
            },
            offset: 0,
            size: MAX_RESULTS,
        };
        let reply = fetch.post(&call).await?;
        let page = api::parse_reply(reply.status, reply.retry_after.as_deref(), &reply.body)?;
        if relevance.is_none() {
            let model = Relevance::new(&series, &episode, &page.results);
            for query in candidate_queries(&series, &episode, &model.keywords()) {
                if !queries.contains(&query) {
                    queries.push(query);
                }
            }
            relevance = Some(model);
        }
        let model = relevance.as_ref().expect("first reply builds relevance");
        let mut ranked = page
            .results
            .iter()
            .take(MAX_RESULTS)
            .filter(|entry| keep(config, entry))
            .filter_map(|entry| Some((model.score(entry)?, build_result(entry)?)))
            .collect::<Vec<_>>();
        ranked.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        if !ranked.is_empty() {
            return Ok(PageOutcome {
                response: SearchResponse {
                    results: ranked
                        .into_iter()
                        .take(page_size(request))
                        .map(|(_, mut result)| {
                            // Only the ID fallback has verified both the series
                            // and episode title. Carry that resolved identity in
                            // the release name so the host can assign coverage.
                            if !series.is_empty()
                                && !episode.is_empty()
                                && let (Some(season), Some(episode_number)) =
                                    (request.season, request.episode)
                            {
                                result.provider_extra.insert(
                                    "original_title".into(),
                                    serde_json::json!(result.title),
                                );
                                result.title = format!(
                                    "{} S{season:02}E{episode_number:02} - {}",
                                    series.join(" "),
                                    result.title,
                                );
                            }
                            result
                        })
                        .collect(),
                    ..SearchResponse::default()
                },
                ceiling_reached: false,
            });
        }
        index += 1;
    }
    Ok(PageOutcome {
        response: SearchResponse::default(),
        ceiling_reached: false,
    })
}

pub fn page_size(request: &SearchRequest) -> usize {
    if request.limit == 0 {
        MAX_RESULTS
    } else {
        request.limit.min(MAX_RESULTS)
    }
}

/// Whether the configuration keeps this entry.
pub fn keep(config: &MvwConfig, entry: &Entry) -> bool {
    if !config.allows_channel(&entry.channel) {
        return false;
    }
    let variant = variant(entry);
    if variant.audio_description && !config.include_audio_description {
        return false;
    }
    if variant.sign_language && !config.include_sign_language {
        return false;
    }
    if let (Some(minimum), Some(duration)) = (config.min_duration_seconds, entry.duration)
        && duration > 0
        && duration < minimum
    {
        return false;
    }
    true
}

pub async fn run_page<F: Fetch>(
    config: &MvwConfig,
    request: &SearchRequest,
    fetch: &F,
) -> Result<PageOutcome, MvwError> {
    if has_external_id(request) {
        return run_fallback(config, request, fetch).await;
    }
    let query = build_query(request);
    if query.is_empty() {
        // No query: a recent-releases or id-only request this indexer does
        // not serve. No call is made.
        return Ok(PageOutcome {
            response: SearchResponse::default(),
            ceiling_reached: false,
        });
    }

    let cursor = match request.page_cursor.as_deref() {
        None => None,
        Some(raw) => Some(PageCursor::decode(raw).ok_or(MvwError::InvalidCursor)?),
    };
    let pages_read = cursor.map_or(0, |cursor| cursor.pages_read);
    if pages_read >= PAGE_CEILING {
        // A cursor already at the ceiling reads nothing more.
        return Ok(PageOutcome {
            response: SearchResponse::default(),
            ceiling_reached: true,
        });
    }

    let size = page_size(request);
    let offset = cursor.map_or(0, |cursor| cursor.offset);
    let call = QueryCall {
        query,
        // One allowed channel narrows the query itself, so a page is not
        // spent on other channels; a longer list is filtered below.
        channel: match config.channels.as_slice() {
            [only] => Some(only.clone()),
            _ => None,
        },
        offset,
        size,
    };
    let reply = fetch.post(&call).await?;
    let result = api::parse_reply(reply.status, reply.retry_after.as_deref(), &reply.body)?;

    let read = result.results.len();
    let results = result
        .results
        .iter()
        .filter(|entry| keep(config, entry))
        .filter_map(build_result)
        .collect::<Vec<_>>();

    let next_offset = offset.saturating_add(read);
    let total = result
        .query_info
        .as_ref()
        .and_then(|info| info.total_results);
    let more = read >= size
        && next_offset <= MAX_OFFSET
        && total.is_none_or(|total| (next_offset as u64) < total);

    let pages_read = pages_read.saturating_add(1);
    let mut ceiling_reached = false;
    let next_cursor = if !more {
        None
    } else if pages_read >= PAGE_CEILING {
        ceiling_reached = true;
        None
    } else {
        Some(
            PageCursor {
                pages_read,
                offset: next_offset,
            }
            .encode(),
        )
    };

    Ok(PageOutcome {
        response: SearchResponse {
            results,
            next_cursor,
            ..SearchResponse::default()
        },
        ceiling_reached,
    })
}
