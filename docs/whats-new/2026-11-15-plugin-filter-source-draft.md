### Plug-in filter source origin and draft indicators
- Plug-in pixel filters receive source decode facts (origin and bit depth) and draft scale indicators in `_image`, allowing negative converters to correctly handle untagged linear scans and adapt during interactive drafts (issue #1211).
