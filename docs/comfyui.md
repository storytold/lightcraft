# Replace or remove masked areas with ComfyUI

LightCraft can use an independently operated ComfyUI server to replace pixels inside an existing
mask, or to remove what the mask covers and rebuild the background ([Remove](#remove)). This is
optional, desktop-only, and off until you press **Generate** or **Remove**. No model is downloaded
and no service starts automatically. You need a running ComfyUI server with either the Qwen
Image 2.1 GGUF preset below or an SDXL checkpoint. LightCraft selects the Qwen preset for `.gguf` filenames and the SDXL preset for
other filenames; this is not support for arbitrary GGUF model architectures.

## In LightCraft

1. Open a photo and create/select a mask in **Masking** (SAM 3 Object/Describe, brush, gradient,
   range or combined masks all work). White in the mask preview is the replacement area.
2. Expand **Generate with ComfyUI** below the mask-selection controls, keep **Replace** selected,
   then enter what should
   appear in that area (and, optionally, a seed). **Prompt sent to ComfyUI** shows the exact
   positive prompt the server will receive.
3. Once, open **Settings ▸ ComfyUI** (or **ComfyUI settings…** in the panel). Enter your server URL
   (for example `http://127.0.0.1:8188`, or your workstation's LAN address) and the exact installed
   model filename. For the installed Qwen preset, use `qwen-image-2.1-UC-BF16.gguf`. SDXL remains
   available with a checkpoint such as `Juggernaut-XL_v9_RunDiffusionPhoto_v2.safetensors`. The
   same page holds the negative prompt (SDXL only) and the **Qwen instruction** (below).
4. Press **Generate**. The activity list shows the background task and its cancel button.
5. Inspect the preview. **Accept replacement** saves/imports a new PNG and stacks it above the
   original. **Try again** increments the seed and generates again for the original photo/mask
   (available while that photo is selected). **Discard**, or closing the preview, changes nothing
   in the library. Errors appear beneath the prompt; fix the settings and generate again.

The prompt, server, model, negative prompt, seed and Qwen instruction are retained with the app's
UI preferences. They are also available through `ui.set` under `generation` (the instruction as
`qwenInstruction`) and as `mask.generate` parameters. No requests are made when selecting
a mask, opening settings, starting LightCraft or reading generation status.

## Image handling

- The photo and mask are rendered in the same cropped, oriented, corrected frame, with the
  current develop settings. Add/subtract/intersect components, inversion, refinement and Amount
  are evaluated by the same pipeline as the mask overlay.
- LightCraft sends only the mask's bounding region with context, resized to at most 1024 pixels
  per side (multiples of 32, shared by both VAEs). A small selection can therefore use more model detail than a
  whole-photo resize. Large selections are still limited to the model's resolution.
- The bundled **Lightcraft-masking** API templates bind the photo, mask, prompt, negative
  prompt, model and seed. SDXL uses its checkpoint, CLIP and inpaint VAE nodes. Qwen uses
  reference-image conditioning with the original photo and the combined mask as separate
  references, the encoded original and a latent noise mask. The instruction explicitly asks
  Qwen to replace the entire white area, including all original objects there. Both decode to RGB for local compositing. The
  templates are independent of a workflow edited/saved in the browser. ComfyUI's implementation
  and custom-node code are not bundled into LightCraft.
- The returned region is scaled back and blended once, in linear light, through the original
  full-resolution mask. Pixels whose evaluated mask value is zero remain byte identical to the
  developed 8-bit render. The new photo retains the rendered photo's full dimensions.
- Accept writes a new **8-bit sRGB PNG**, plus a `.generation.json` recipe with the prompt, seed,
  checkpoint, original develop settings, mask and region. The original RAW/image and edits stay
  intact. The copy has the develop adjustments baked in; it is not a generated layer on the RAW.
  It does not inherit all original embedded EXIF/IPTC metadata.

## Qwen Image 2.1 preset

This preset follows the user's existing ComfyUI Qwen setup. Install/configure models in ComfyUI
independently; LightCraft does not download weights or install custom nodes. The preset requires:

- Diffusion model: `qwen-image-2.1-UC-BF16.gguf` in `models/diffusion_models/` (or the exact
  equivalent Qwen 2.1 GGUF filename entered in settings).
- Text encoder: `qwen3vl_8b_bf16.safetensors` in `models/text_encoders/`.
- VAE: `qwen_image_2.1_vae_bf16.safetensors` in `models/vae/`.
- The already installed `UnetLoaderGGUF` node, plus ComfyUI's `ModelComputeDtype`,
  `QwenImage21Cache` and `TextEncodeQwenImage21` nodes.

Sampling uses BF16 computation, automatic cache placement, 25 Euler/simple steps and CFG 1.
At CFG 1 **negative prompts have no effect**; leave that field empty. Prompt enhancement is not
run: Qwen receives the **Qwen instruction** from Settings ▸ ComfyUI with `{prompt}` replaced by
the prompt typed in the panel. The default names the two references and asks Qwen to replace the
entire white region and keep the black context; edit it freely, but it must keep `{prompt}`
(**Reset to default** restores it). The accepted image's JSON recipe records the exact prompt sent
(`sentPrompt`). Qwen may take longer than SDXL; actual
quality depends on the scene, selection and prompt. SAM 3 still creates the mask locally.

The source and combined mask (all Add/Subtract/Intersect components evaluated together) are
uploaded at matching dimensions. Qwen sees both as references: the source gives scene context
and the white-on-black mask identifies the entire replacement area. A region only constrains
where editing is allowed; it does not guarantee that the model follows the instruction perfectly. LightCraft keeps compositing
through the original full-resolution mask, so the model cannot alter zero-mask pixels. The saved
browser workflow performs its own composite for standalone use; the application API template
returns the raw RGB region to avoid blending a feathered edge twice.

## Limits and cancellation

This first version supports SDR photos up to 80 megapixels and the bundled Qwen 2.1 GGUF and SDXL workflows.
It does not offer arbitrary custom workflow bindings, HDR generation or lossless RAW editing.
Each ComfyUI request (a replacement, removal's coarse pass, or one detail tile) has a ten-minute limit, and individual network requests have a thirty-second timeout.
URLs may use HTTP or HTTPS without embedded credentials; authenticated gateways are not yet
supported. Redirects and environment proxies are disabled to keep uploads at the chosen server.

Cancelling discards the local result and removes this job from ComfyUI's pending queue. A job
already executing may finish on the server; LightCraft never sends the global `/interrupt`
request, which could interrupt another client's job. Uploaded crops/masks and generated outputs
remain on your ComfyUI server under its normal retention policy.

## Remove

**Remove** (next to Replace in the panel) takes what the mask covers out of the scene. **What to
remove** (its own field, `subject`, separate from Replace's prompt) names it ("the person in the
yellow jacket"); left empty, the mask's name is used, so a mask
named by Describe or renamed by you needs nothing typed. A default name ("Mask 3") says nothing
about the content and is refused. Naming the subject matters: asked only to clear "the white
region", Qwen tends to redraw what is there. Removal runs in two passes:

1. **Coarse pass: wide context.** The model sees the mask with much more of its surroundings
   (**Context**, default 2.5× the selection's longer side, at least 32 px each way, shifted to
   stay inside the photo), resized to at most 1024 px. Seeing the lines, perspective and textures
   around the object lets it rebuild what was behind it. Qwen receives the **Removal
   instruction** (Settings ▸ ComfyUI) with `{prompt}` replaced by what to remove; SDXL receives a
   plain "empty background" prompt, with what to remove and object/person/text in its negative
   prompt.
2. **Detail pass: full resolution.** A wide crop of a 20 MP photo reaches the model scaled down
   several times, so the coarse fill alone is soft at 100 %. When the crop was scaled down by more
   than 1.25×, LightCraft covers the selection with 1024 × 1024 tiles at full resolution
   (overlapping by an eighth, only where the mask has pixels, at most 36; a larger selection gets
   proportionally larger tiles instead). Each tile starts from the result so far and is redrawn at
   **Detail** strength (default 0.35; 0 skips this pass), with its own seed (seed + tile number)
   and the **Removal detail instruction**. Tiles are blended in linear light, ramping in from their
   inner edges, so neighbours and the coarse fill meet without seams. SDXL uses a separate detail
   workflow (`Lightcraft-refine`) that encodes the tile instead of blanking the mask; Qwen's
   workflow already encodes its input.

**Grow edge** (default 8 px at full resolution) widens the mask first, so the object's outline
and halo go too; widen it, or paint the shadow into the mask, to take a shadow with it. Every pass
is blended through that grown mask: unselected pixels stay byte identical. The activity row counts
the requests (one, plus one per tile). The recipe (`.generation.json`) records the workflow
`Lightcraft-remove`, what was removed, the selection, context crop, tiles, tile seeds and the prompts sent.

## Commands

All frontends use the same engine commands (control channel and MCP included):

```json
{"command":"mask.generate","params":{"endpoint":"http://127.0.0.1:8188","model":"Juggernaut-XL_v9_RunDiffusionPhoto_v2.safetensors","prompt":"a blue ceramic vase with yellow flowers","seed":42}}
{"command":"mask.generate","params":{"mode":"remove","subject":"the person in the yellow jacket","context":2.5,"grow":8,"detail":0.35,"seed":7}}
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
