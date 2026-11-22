### A damaged library settings file is kept and reported

- If a library's `catalog-settings.json` can't be parsed, it is moved aside as `catalog-settings.json.corrupt-<time>`, the settings of a new library (backups on) are used, and a notice says so when the library opens. If it can't be read at all, it is left alone and never overwritten. Before, the defaults were used silently, with backups off.
- A damaged `recent-catalogs.json` is moved aside the same way, and one that can't be read is never written over; when an older version's recent list can't be carried over, a notice says so.
