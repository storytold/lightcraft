## Develop

- **Plug-in filter source and draft metadata:** pixel filter plug-ins now receive `source` (origin, bits per sample, float) and `draft` status (including draft scale and native canvas dimensions) in their `_image` parameters, allowing filters to identify untagged linear scans and adapt during interactive slider drags. Issue #1211.
