## Performance

- **Noise reduction drags on the CPU:** a Luminance / Detail drag keeps the colour noise reduction and reruns only
  luminance NR (draft 1152×768 of a 24 MP raw: 63.8 → 33.4 ms on a loaded Windows machine); release renders and
  exports are unchanged. `cargo xtask bench --strict` also fails on missed wall-time budgets.
