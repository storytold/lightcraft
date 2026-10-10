# Publish services

> Names in angle brackets are the values set in [`brand.toml`](../../brand.toml); see the [manual index](README.md).

A publish service keeps a destination in step with collections, as in Lightroom Classic. Each service has a kind,
a name and its own export settings, and holds **published collections**. A published collection is an ordinary album
(inside a collection set named after the service), so you add and remove photos as with any album. Every photo in
it is **new**, **modified** (edited since it was published), **published**, or **to remove** (taken out of the
collection); Publish sends the new and modified ones and removes the rest.

Kinds today:

| Kind | What it does |
|---|---|
| Hard Drive (`hardDrive`) | renders into a folder you choose |
| Immich (`immich`) | uploads to a connected Immich server ([immich.md](../immich.md)) |
| Plug-ins (`plugin:<id>`) | services provided by installed plug-ins ([plugins.md](../plugins.md), `plugin.publishServices`) |

## In the app

The Publish Services panel in Library: Set Up Publish Service…, then Create Published Collection…, drag photos in,
and Publish. Edit a photo and it moves to *Modified*; Mark as Up-to-Date accepts the change without republishing.
Deleting a service or collection forgets what was published; the published files stay.

## Commands

```sh
<cli> run --library DIR \
  publish.createService name=Portfolio dir=/srv/portfolio \
  publish.createCollection service=Portfolio name=Best addSelected=true \
  publish.run service=Portfolio
```

`publish.services` lists services and collection counts, `publish.status collection=<albumId>` the state of each
photo, `publish.comments` comments where the service supports them. The configuration lives in `publish.json` in
the library folder; what was published where is recorded in the catalog.

Not yet: Flickr, SmugMug and other online services (a plug-in can add one).
