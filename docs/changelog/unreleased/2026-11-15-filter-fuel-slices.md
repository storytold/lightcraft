## Fixes

- **Plug-in filters no longer fail at a fuel slice:** filter modules are compiled when loaded, as plug-in modules already are, instead of function by function as they first run. A first run that fell at the end of one of the host's fuel slices failed the band with "instruction budget", and with bands in parallel that changed from run to run (#1200).
