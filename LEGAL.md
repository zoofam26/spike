# Legal & Acceptable Use

**Spike is a streaming *player* and buffering engine. It ships with no media content,
no source lists beyond two or three official free channels, and no mechanism for
discovering or obtaining copyrighted streams.**

## Your responsibilities

- Only point Spike at streams you have the **legal right to access**: your own IPTV
  subscription's playlist, feeds from rights-holders, or channels that are officially
  offered free by their operators (the bundled sample contains only this third kind).
- Respect the terms of service of every source you configure. Many sports broadcasts are
  geofenced and/or subscription-only; circumventing those restrictions is **not**
  something Spike supports or permits.

## What Spike will never do

- **No DRM circumvention.** The HLS parser explicitly rejects encrypted streams
  (`#EXT-X-KEY` with `METHOD=AES-128`, `SAMPLE-AES`, …) instead of decrypting them.
- **No piracy source aggregation.** Spike does not ship, scrape, or bundle playlists of
  unauthorized restreams, and the maintainers will not accept contributions that add
  such functionality.
- **No license checks to bypass.** The local re-serve server only re-serves content the
  user's own machine already fetched from a source the user configured.

## Reverse engineering

Spike's development is **clean-room with respect to proprietary streaming apps**: no
code, manifests, endpoints, or logic were extracted from third-party APKs. The HLS,
M3U, and XMLTV handling is implemented from public, open specifications (Apple's HLS
authoring spec, the extended-M3U/IPTV community format, the XMLTV DTD).

## If you redistribute Spike

You are responsible for how your build is used. The MIT license gives no warranty and
does not shield users or redistributors from liability arising from unlawful use.
Take-down requests concerning *content* should be addressed to whoever operates the
upstream source; Spike does not host or cache media beyond the user's local RAM window.
