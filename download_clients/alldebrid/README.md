# AllDebrid

AllDebrid download-client plugin for Scryer. AllDebrid performs the torrent
retrieval; a configured local download backend then transfers the ready file
links to storage accessible to Scryer. The first backend is pyLoad-ng.

## Configuration

- `alldebrid_api_key`: AllDebrid API bearer key.
- `pyload_url`: pyLoad-ng base URL reachable by Scryer.
- `pyload_api_key`: pyLoad-ng API key, sent in `X-API-Key`.
- `download_root`: the pyLoad output root as mounted inside Scryer. Keep the
  pyLoad package output under this root so Scryer can import it.

Create a pyLoad-ng API key for an account permitted to add packages and inspect
the queue. The plugin uses the current REST API (`/api/add_package`,
`/api/get_package_data`, and `/api/get_server_version`), not the obsolete
`/api/login` endpoint.

The plugin uses AllDebrid's magnet upload/status/files APIs and hands ready
file links to pyLoad-ng. It polls pyLoad-ng for completion and exposes the
package directory to Scryer. The pyLoad output directory must be mounted and
readable by Scryer. This adapter does not provide torrent seeding: AllDebrid
handles the torrent remotely, and pyLoad downloads the resulting file links.
