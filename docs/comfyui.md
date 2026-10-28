# Generate a replacement with ComfyUI

LightCraft can use an independently operated ComfyUI server to replace pixels inside an existing
mask. This is optional, desktop-only, and off until you press **Generate**. No model is downloaded
and no service starts automatically. You need a running ComfyUI server with an SDXL checkpoint
and the built-in nodes used by `crates/engine/src/comfyui/Lightcraft-masking-api.json`.

## In LightCraft

1. Open a photo and create/select a mask in **Masking** (SAM 3 Object/Describe, brush, gradient,
   range or combined masks all work). White in the mask preview is the replacement area.
2. Expand **Generate replacement** below the mask-selection controls, then enter what should appear in that area.
3. Expand **ComfyUI settings**. Enter your server URL (for example `http://127.0.0.1:8188`, or your
   workstation's LAN address) and the exact installed checkpoint filename. The default checkpoint
   name is `Juggernaut-XL_v9_RunDiffusionPhoto_v2.safetensors`; change it if yours differs.
4. Press **Generate**. The activity list shows the background task and its cancel button.
5. Inspect the preview. **Accept replacement** saves/imports a new PNG and stacks it above the
   original. **Try again** increments the seed and generates again for the original photo/mask
   (available while that photo is selected). **Discard**, or closing the preview, changes nothing
   in the library. Errors appear beneath the prompt; fix the settings and generate again.

The prompt, server, model, negative prompt and seed are retained with the app's UI preferences.
They are also available through `ui.set` under `generation`. No requests are made when selecting
a mask, opening settings, starting LightCraft or reading generation status.

## Image handling

- The photo and mask are rendered in the same cropped, oriented, corrected frame, with the
  current develop settings. Add/subtract/intersect components, inversion, refinement and Amount
  are evaluated by the same pipeline as the mask overlay.
- LightCraft sends only the mask's bounding region with context, resized to at most 1024 pixels
  per side (multiples of eight). A small selection can therefore use more model detail than a
  whole-photo resize. Large selections are still limited to the model's resolution.
- The bundled **Lightcraft-masking** API template uses native ComfyUI nodes: checkpoint loader,
  image/mask loaders, CLIP text encoding, inpaint VAE encoding, KSampler, VAE decoding and image
  saving. Photo, mask, prompt, negative prompt, model and seed are bound by LightCraft. This
  bundled template is independent of a workflow edited/saved in the browser. No custom nodes
  are installed, and ComfyUI's implementation is not bundled into LightCraft.
- The returned region is scaled back and blended once, in linear light, through the original
  full-resolution mask. Pixels whose evaluated mask value is zero remain byte identical to the
  developed 8-bit render. The new photo retains the rendered photo's full dimensions.
- Accept writes a new **8-bit sRGB PNG**, plus a `.generation.json` recipe with the prompt, seed,
  checkpoint, original develop settings, mask and region. The original RAW/image and edits stay
  intact. The copy has the develop adjustments baked in; it is not a generated layer on the RAW.
  It does not inherit all original embedded EXIF/IPTC metadata.

## Limits and cancellation

This first version supports SDR photos up to 80 megapixels and the bundled SDXL workflow.
It does not offer arbitrary custom workflow bindings, HDR generation or lossless RAW editing.
Generation has a ten-minute limit and individual network requests have a thirty-second timeout.
URLs may use HTTP or HTTPS without embedded credentials; authenticated gateways are not yet
supported. Redirects and environment proxies are disabled to keep uploads at the chosen server.

Cancelling discards the local result and removes this job from ComfyUI's pending queue. A job
already executing may finish on the server; LightCraft never sends the global `/interrupt`
request, which could interrupt another client's job. Uploaded crops/masks and generated outputs
remain on your ComfyUI server under its normal retention policy.

## Commands

All frontends use the same engine commands (control channel and MCP included):

```json
{"command":"mask.generate","params":{"endpoint":"http://127.0.0.1:8188","model":"Juggernaut-XL_v9_RunDiffusionPhoto_v2.safetensors","prompt":"a blue ceramic vase with yellow flowers","seed":42}}
{"command":"mask.generationStatus","params":{}}
{"command":"mask.acceptGeneration","params":{}}
{"command":"mask.discardGeneration","params":{}}
```

`mask.generate` uses the active photo and selected mask; an explicit `maskId` can select another
mask on that photo. It returns immediately. Poll status until `running` is false; `ready` means
there is a preview to accept. Accept rejects a result if its original library/photo is no longer
open. No files are saved or imported until Accept.

The transport follows ComfyUI's [documented HTTP API](https://docs.comfy.org/development/comfyui-server/comms_routes):
`/upload/image`, `/prompt`, `/history/{prompt_id}`, `/view`, and targeted queue deletion.
