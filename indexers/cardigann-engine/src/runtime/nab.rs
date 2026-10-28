use super::{Definition, PluginSearchRequest, Value, Variables};
use scryer_plugin_sdk::PluginNabOperation;

pub(super) fn search_type(request: &PluginSearchRequest) -> String {
    match request.nab.as_ref().map(|nab| nab.operation) {
        Some(PluginNabOperation::Search) => "search",
        Some(PluginNabOperation::Tv) => "tv-search",
        Some(PluginNabOperation::Movie) => "movie-search",
        Some(PluginNabOperation::Music) => "music-search",
        Some(PluginNabOperation::Book) => "book-search",
        None if request.recent_paging => "search",
        None => return super::cardigann_search_type(request.facet.as_deref()),
    }
    .to_string()
}

// Definition capability keys use `tv-search`/`movie-search`, whereas Prowlarr
// binds the incoming wire operation (`tvsearch`/`movie`) to `.Query.Type`.
pub(super) fn query_type(request: &PluginSearchRequest) -> String {
    match search_type(request).as_str() {
        "tv-search" => "tvsearch",
        "movie-search" => "movie",
        "music-search" => "music",
        "book-search" => "book",
        _ => "search",
    }
    .to_string()
}

fn exact(request: &PluginSearchRequest) -> bool {
    request.nab.is_some() || request.recent_paging
}

// Prowlarr's HttpIndexerBase rejects pagination for Cardigann. Preserve the
// definition's complete search-path chain and let Scryer's raw result buffer
// page those rows. Never advertise single-provider-page cursor support.
pub(super) fn request_is_supported(
    definition: &Definition,
    request: &PluginSearchRequest,
) -> Result<bool, String> {
    if !exact(request) {
        return Ok(true);
    }
    if request.recent_paging
        && request
            .nab
            .as_ref()
            .is_some_and(|nab| nab.operation != PluginNabOperation::Search)
    {
        return Err("Cardigann recent paging requires a basic search operation".into());
    }
    if request.page_cursor.is_some() || request.rss_catch_up.is_some() {
        return Err("Cardigann Nab requests cannot carry a provider cursor or RSS catch-up".into());
    }
    let mode = search_type(request);
    let modes = definition
        .caps
        .get("modes")
        .and_then(serde_yaml::Value::as_mapping);
    if !modes.is_some_and(|modes| modes.contains_key(serde_yaml::Value::String(mode.clone()))) {
        return Ok(false);
    }
    let parameters = super::caps_mode_parameters(definition, &mode);
    let supports = |name: &str| {
        parameters
            .iter()
            .any(|value| value.trim().eq_ignore_ascii_case(name))
    };
    if !request.query.trim().is_empty() && !supports("q") {
        return Ok(false);
    }
    if request.ids.iter().any(|(key, value)| {
        !value.trim().is_empty() && !super::cardigann_id_parameter(key).is_some_and(supports)
    }) {
        return Ok(false);
    }
    let Some(nab) = &request.nab else {
        return Ok(true);
    };
    Ok(nab.parameters.iter().all(|(key, value)| {
        binding(nab.operation, key).is_some() && (value.trim().is_empty() || supports(key))
    }))
}

fn binding(operation: PluginNabOperation, parameter: &str) -> Option<&'static str> {
    use PluginNabOperation::{Book, Movie, Music, Tv};
    match (operation, parameter.to_ascii_lowercase().as_str()) {
        (Movie | Tv | Music | Book, "year") => Some("Year"),
        (Movie | Tv | Music | Book, "genre") => Some("Genre"),
        (Movie | Tv, "imdbid") => Some("IMDBID"),
        (Movie | Tv, "tmdbid") => Some("TMDBID"),
        (Movie | Tv, "traktid") => Some("TraktID"),
        (Movie | Tv, "doubanid") => Some("DoubanID"),
        (Tv, "tvdbid") => Some("TVDBID"),
        (Tv, "tvmazeid") => Some("TVMazeID"),
        (Tv, "rid") => Some("TVRageID"),
        (Tv, "season") => Some("Season"),
        (Tv, "ep") => Some("Ep"),
        (Music, "album") => Some("Album"),
        (Music, "artist") => Some("Artist"),
        (Music, "label") => Some("Label"),
        (Music, "track") => Some("Track"),
        (Book, "author") => Some("Author"),
        (Book, "title") => Some("Title"),
        (Book, "publisher") => Some("Publisher"),
        _ => None,
    }
}

fn set(variables: &mut Variables, key: &str, value: String) {
    variables.insert(
        format!(".Query.{key}"),
        if value.is_empty() {
            Value::Null
        } else {
            Value::String(value)
        },
    );
}

fn text<'a>(variables: &'a Variables, key: &str) -> &'a str {
    variables
        .get(&format!(".Query.{key}"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

// Matches Prowlarr CardigannRequestGenerator's operation-specific variables
// and keyword assembly, without changing Scryer's derived-title search path.
pub(super) fn bind_query_variables(
    request: &PluginSearchRequest,
    variables: &mut Variables,
) -> String {
    if !exact(request) {
        return request.query.clone();
    }
    for key in [
        "Movie",
        "Series",
        "Artist",
        "Album",
        "Label",
        "Track",
        "Author",
        "Title",
        "Publisher",
        "Genre",
        "Season",
        "Ep",
        "Episode",
        "Extended",
        "APIKey",
    ] {
        variables.insert(format!(".Query.{key}"), Value::Null);
    }
    // A media context is a derived-search hint, not an exact protocol selector.
    set(variables, "Year", String::new());
    if let Some(nab) = &request.nab {
        for (key, value) in &nab.parameters {
            if let Some(field) = binding(nab.operation, key) {
                if field == "IMDBID" {
                    set(variables, "IMDBID", super::full_imdb_id(value));
                    set(variables, "IMDBIDShort", super::short_imdb_id(value));
                } else {
                    set(variables, field, value.clone());
                }
            }
        }
        if nab.operation == PluginNabOperation::Tv {
            let episode = episode_search_string(text(variables, "Season"), text(variables, "Ep"));
            set(variables, "Episode", episode);
        }
    }
    let keywords = ["Q", "Series", "Movie", "Year", "Episode"]
        .into_iter()
        .map(|key| text(variables, key))
        .filter(|value| !value.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    set(variables, "Keywords", keywords.clone());
    keywords
}

fn episode_search_string(season: &str, episode: &str) -> String {
    let Ok(season) = season.parse::<u32>() else {
        return String::new();
    };
    if season == 0 {
        return String::new();
    }
    if let Some((month, day)) = episode.split_once('/')
        && let (Ok(year), Ok(month), Ok(day)) = (i32::try_from(season), month.parse(), day.parse())
        && let Some(date) = chrono::NaiveDate::from_ymd_opt(year, month, day)
    {
        return date.format("%Y.%m.%d").to_string();
    }
    if episode.trim().is_empty() {
        return format!("S{season:02}");
    }
    match episode.parse::<u32>() {
        Ok(episode) => format!("S{season:02}E{episode:02}"),
        Err(_) => format!("S{season:02}E{episode}"),
    }
}
