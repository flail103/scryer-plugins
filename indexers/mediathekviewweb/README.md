# MediathekViewWeb Indexer

Searches a MediathekViewWeb server through its `/api/query` endpoint and returns
public broadcaster media listings as generic HTTP(S) downloads. Configure the
instance URL; the public service is used by default.

The legacy Scryer SDK contract carries one download URL. The plugin selects the
HD rendition when available, otherwise the standard or low rendition. Other
renditions are retained in `provider_extra.download_options` and encoded in the
forward-compatible `provider_extra.download_resources` shape for hosts that
support selectable resources. Subtitle URLs remain exposed through the SDK's
subtitle list and are also marked as sidecars in that resource shape. Older
hosts ignore the additional metadata and continue using the single selected
download URL.
