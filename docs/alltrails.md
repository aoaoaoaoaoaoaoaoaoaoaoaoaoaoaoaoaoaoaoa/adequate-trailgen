# AllTrails Navigation Handoff

Verified against [AllTrails’ official upload instructions](https://support.alltrails.com/hc/en-us/articles/37228498475028-Uploading-files-to-AllTrails) on 2026-08-02.

Trailgen’s supported boundary is a manual GPX handoff. It does not automate an AllTrails account or private API.

1. Finish the route and save it to the project Library.
2. Press `➚` beside its name under **Saved Trails** and choose a `.gpx` destination.
3. In the AllTrails mobile app, open **Saved → Lists → Custom routes & maps**, use the overflow menu, and select **Upload route**. On the website, use **Explore → Build custom route → Upload a route**.
4. Open the resulting custom route in the mobile app and use Navigate. Download its map before departure if offline navigation is required.

AllTrails currently accepts GPX tracks and routes on web and mobile, with a documented 20 MB upload limit. Trailgen emits one contiguous GPX 1.1 hiking track with the saved name, route geometry, available elevations, and compact measurements. It deliberately omits timestamps because this is a planned route, not a recorded activity.

Export requires a saved trail. Search candidates are transient, and an unfinished editor draft has not crossed the Library’s durability boundary. Save-first therefore prevents a navigational file from claiming identity the project itself has not retained.

The debug equivalent is:

```sh
trailgen saved PROJECT
trailgen export PROJECT --trail NAME_OR_ID --output route.gpx
```

Both frontends invoke the same Library reader and GPX serializer. There is no separate AllTrails bridge, format policy registry, generated-candidate snapshot, or write-back client.
