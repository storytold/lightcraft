## Develop

- **Plug-in filters remember settings:** a filter parameter marked `"remember": true` in the manifest (ABI v1.1 proposal) starts each new copy of the filter (`develop.pluginFilter.add`, `addTo`, the filter's key) at the value last set for it, kept per plug-in in `filter-remembered.json` in the settings folder; values given to `add` win, the other parameters start at their defaults.
