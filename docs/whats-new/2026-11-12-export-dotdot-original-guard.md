### Exports refuse a path that reaches an original through a folder that does not exist yet

- A destination such as `missing/../IMG_1.jpg` named a photo's original once the export created `missing`. Export destinations with a `..` that cannot be resolved up front are now refused, so an original cannot be replaced that way.
