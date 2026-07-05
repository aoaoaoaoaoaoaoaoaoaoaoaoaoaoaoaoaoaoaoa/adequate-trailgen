# AllTrails Import And Reinjection Status

Checked against official AllTrails support material on 2026-07-04. AllTrails is useful for discovery, seed routes, popularity hints, and manual exchange. The core model is provider-agnostic and does not depend on private AllTrails APIs.

Supported now:

- import user-supplied AllTrails GPX exports via `trailgen import-seed --route file.gpx` or `trailgen rate --route file.gpx`
- import user-supplied GeoJSON, KML, and CSV route/network files
- export generated routes as GPX, GeoJSON, KML, and KMZ
- expose `ManualAllTrailsBridge` capabilities for future connectors

Not implemented:

- direct write-back to an AllTrails account
- private API automation

Official support pages describe a sanctioned manual upload path: Build custom route → Upload a route on the website, or Saved → Custom routes → Upload route in mobile apps. AllTrails lists GPX, KML, KMZ, CSV, and many other formats as uploadable. Official support also documents downloads from activities, custom routes, and trail pages, including GPX route/track, GeoJSON track, JSON track, KML, and KMZ.

Current best workflow: export a generated `routes/candidate-N.gpx`, `routes/candidate-N.kml`, or `routes/candidate-N.kmz` file and use AllTrails’ manual upload path. If AllTrails publishes a documented route-create/import API, it should be added behind the `AllTrailsBridge` trait, leaving graph construction and optimization untouched.

Official references:

- <https://support.alltrails.com/hc/en-us/articles/37228498475028-Uploading-files-to-AllTrails>
- <https://support.alltrails.com/hc/en-us/articles/37230403315476-Downloading-files-from-AllTrails>
- <https://support.alltrails.com/hc/en-us/sections/360006411352-Importing-and-exporting-files>
