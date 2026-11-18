## Performance

- **Large libraries open in seconds:** the search for edited photos (whose previews a full thumbnail cache keeps longest) now runs in the background after the library opens, as **Finding edited photos** in the activity stack, and looks each camera up once instead of once per photo. A synthetic 300,000-photo library with a 16.5 GB thumbnail cache opened in 65–70 s and now opens in about 4 s. Issue #979.
