### A damaged library settings file is kept and reported

- If a library's `catalog-settings.json` can't be parsed, it is moved aside as `catalog-settings.json.corrupt-<time>`, the settings of a new library (backups on) are used, and a notice says so when the library opens. Before, the defaults were used silently, with backups off.
