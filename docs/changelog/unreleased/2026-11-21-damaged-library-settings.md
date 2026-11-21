## Library

- **Damaged library settings are kept and reported:** a `catalog-settings.json` that doesn't parse is set aside, a
  new library's settings (backups on) stand in, and a notice says so when the library opens; one that can't be read
  is never overwritten. `recent-catalogs.json` is handled the same way.
