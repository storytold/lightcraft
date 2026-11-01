# lightcraft-flickr

Flickr uploads for LightCraft, in pure Rust (L1, native only):

- `oauth`: OAuth 1.0a request signing (HMAC-SHA1), checked against the specification's worked example;
- `Client`: Flickr's out-of-band sign-in (`request_token` → `authorize_url` → `access_token`) and signed multipart
  uploads (`upload`), retried only when Flickr certainly didn't take them (never after a lost answer), with a cancel flag, over `lightcraft-fetch`'s HTTPS client;
- `reply`: Flickr's form and XML answers, parsed without trusting them;
- `account`: the saved API key and sign-in (`flickr.json`, atomic, owner-only on Unix).

```rust
let client = Client::new(ApiKey { key, secret });
let request = client.request_token(&cancel)?;
open_in_browser(&client.authorize_url(&request));
let token = client.access_token(&request, "123-456-789", &cancel)?;
let id = client.upload(&token, &PhotoInfo { title: "Dunes".into(), ..Default::default() }, "dunes.jpg", &jpeg, &cancel)?;
```

The engine turns catalog photos into uploads (`crates/engine/src/flickr.rs`); see [docs/flickr.md](../../docs/flickr.md).
