# Decypharr

Official Scryer download-client adapter for
[Decypharr](https://decypharr.com/). It uses Decypharr's
qBittorrent-compatible API so Scryer can submit torrents, poll their state,
discover completed content, and remove jobs.

## Configuration

- **base_url**: Decypharr's externally reachable URL, commonly
  `http://localhost:8282`.
- **api_key**: the token from Decypharr's **Settings → Auth** page. This is
  sent as a bearer token and takes precedence over username/password.
- **username** and **password**: optional compatibility-login credentials for
  installations configured to use Decypharr's qBittorrent login endpoint.
- **routing_mode**: apply Scryer's isolation value as a category (default) or
  a tag.
- **static_tags**: optional comma-separated tags applied to every torrent.

The remaining qBittorrent-compatible switches control add behavior and import
handoff. Their availability depends on the Decypharr version in use.

## API scope

The adapter uses `/api/v2/auth/login`, `/api/v2/torrents/info`,
`/api/v2/torrents/add`, and `/api/v2/torrents/delete`, together with the
compatible lifecycle endpoints exposed by the configured Decypharr release.
It does not call Decypharr's configuration, repair, Arr-management, browse, or
token-rotation APIs.
