# Button test checklist (headless, auto-generated)

Every on-screen button, toggle, tab and menu control found in 29 screen states (2 library views x 12 right panels + 5 other views) was clicked by `tools/edit_flow/click_all.py` on the demo library. **1,015 clicks, 0 crashes, 0 errors** (81 first-run misses were harness state leakage and all pass in a fresh process).

Status key: ✅ click delivered, no error/crash. A passing click does not prove the right thing happened; items marked ☐ in the last column still need an eyes-on check on the iPad.

| Control id | Seen in (view/right-panel) | Headless | iPad eyes-on |
|---|---|---|---|
| `button:allMetadata` | photoGrid/info, detail/info | ✅ | ☐ |
| `button:auto` | photoGrid/edit, detail/edit | ✅ | ☐ |
| `button:bw` | photoGrid/edit, detail/edit | ✅ | ☐ |
| `button:copySettings` | photoGrid/none, detail/none, squareGrid/none +4 more | ✅ | ☐ |
| `button:cropFlipH` | photoGrid/crop, detail/crop | ✅ | ☐ |
| `button:cropFlipV` | photoGrid/crop, detail/crop | ✅ | ☐ |
| `button:cropLock` | photoGrid/crop, detail/crop | ✅ | ☐ |
| `button:cropOverlay-diagonal` | photoGrid/crop, detail/crop | ✅ | ☐ |
| `button:cropOverlay-golden` | photoGrid/crop, detail/crop | ✅ | ☐ |
| `button:cropOverlay-grid` | photoGrid/crop, detail/crop | ✅ | ☐ |
| `button:cropOverlay-none` | photoGrid/crop, detail/crop | ✅ | ☐ |
| `button:cropOverlay-spiral` | photoGrid/crop, detail/crop | ✅ | ☐ |
| `button:cropOverlay-thirds` | photoGrid/crop, detail/crop | ✅ | ☐ |
| `button:cropOverlay-triangle` | photoGrid/crop, detail/crop | ✅ | ☐ |
| `button:cropReset` | photoGrid/crop, detail/crop | ✅ | ☐ |
| `button:cropRotateLeft` | photoGrid/crop, detail/crop | ✅ | ☐ |
| `button:cropRotateRight` | photoGrid/crop, detail/crop | ✅ | ☐ |
| `button:cropSwapAspect` | photoGrid/crop, detail/crop | ✅ | ☐ |
| `button:eyeMode-pet` | photoGrid/redEye, detail/redEye | ✅ | ☐ |
| `button:eyeMode-red` | photoGrid/redEye, detail/redEye | ✅ | ☐ |
| `button:findBlemishes` | photoGrid/remove, detail/remove | ✅ | ☐ |
| `button:findDust` | photoGrid/remove, detail/remove | ✅ | ☐ |
| `button:hdr` | photoGrid/edit, detail/edit | ✅ | ☐ |
| `button:keywordPaint` | photoGrid/keywords, detail/keywords | ✅ | ☐ |
| `button:quickDevelopAutoTone` | photoGrid/quickDevelop, detail/quickDevelop | ✅ | ☐ |
| `button:quickDevelopResetAll` | photoGrid/quickDevelop, detail/quickDevelop | ✅ | ☐ |
| `button:removeMode-clone` | photoGrid/remove, detail/remove | ✅ | ☐ |
| `button:removeMode-fill` | photoGrid/remove, detail/remove | ✅ | ☐ |
| `button:removeMode-heal` | photoGrid/remove, detail/remove | ✅ | ☐ |
| `button:removeMode-remove` | photoGrid/remove, detail/remove | ✅ | ☐ |
| `button:reset` | photoGrid/edit, detail/edit | ✅ | ☐ |
| `button:straightenAuto` | photoGrid/crop, detail/crop | ✅ | ☐ |
| `button:straightenTool` | photoGrid/crop, detail/crop | ✅ | ☐ |
| `button:upright-auto` | photoGrid/crop, detail/crop | ✅ | ☐ |
| `button:upright-full` | photoGrid/crop, detail/crop | ✅ | ☐ |
| `button:upright-guided` | photoGrid/crop, detail/crop | ✅ | ☐ |
| `button:upright-level` | photoGrid/crop, detail/crop | ✅ | ☐ |
| `button:upright-off` | photoGrid/crop, detail/crop | ✅ | ☐ |
| `button:upright-vertical` | photoGrid/crop, detail/crop | ✅ | ☐ |
| `button:versionCreate` | photoGrid/versions, detail/versions | ✅ | ☐ |
| `button:versionsTab-auto` | photoGrid/versions, detail/versions | ✅ | ☐ |
| `button:versionsTab-named` | photoGrid/versions, detail/versions | ✅ | ☐ |
| `chip:cropRatio:16:9` | photoGrid/quickDevelop, detail/quickDevelop | ✅ | ☐ |
| `chip:cropRatio:1:1` | photoGrid/quickDevelop, detail/quickDevelop | ✅ | ☐ |
| `chip:cropRatio:2:3` | photoGrid/quickDevelop, detail/quickDevelop | ✅ | ☐ |
| `chip:cropRatio:4:5` | photoGrid/quickDevelop, detail/quickDevelop | ✅ | ☐ |
| `chip:cropRatio:Orig` | photoGrid/quickDevelop, detail/quickDevelop | ✅ | ☐ |
| `chip:quickPreset:B&W` | photoGrid/quickDevelop, detail/quickDevelop | ✅ | ☐ |
| `chip:quickPreset:Landscape` | photoGrid/quickDevelop, detail/quickDevelop | ✅ | ☐ |
| `chip:quickPreset:Portrait` | photoGrid/quickDevelop, detail/quickDevelop | ✅ | ☐ |
| `chip:quickPreset:Vivid` | photoGrid/quickDevelop, detail/quickDevelop | ✅ | ☐ |
| `chip:wbPreset:As Shot` | photoGrid/quickDevelop, detail/quickDevelop | ✅ | ☐ |
| `chip:wbPreset:Auto` | photoGrid/quickDevelop, detail/quickDevelop | ✅ | ☐ |
| `chip:wbPreset:Cloudy` | photoGrid/quickDevelop, detail/quickDevelop | ✅ | ☐ |
| `chip:wbPreset:Daylight` | photoGrid/quickDevelop, detail/quickDevelop | ✅ | ☐ |
| `icon:activity` | photoGrid/none, photoGrid/edit, photoGrid/quickDevelop +26 more | ✅ | ☐ |
| `icon:appearance` | photoGrid/none, photoGrid/edit, photoGrid/quickDevelop +26 more | ✅ | ☐ |
| `icon:autoAdvance` | compare/none, survey/none | ✅ | ☐ |
| `icon:back` | photoGrid/none, photoGrid/edit, photoGrid/quickDevelop +26 more | ✅ | ☐ |
| `icon:beforeAfter` | detail/none, detail/edit, detail/quickDevelop +10 more | ✅ | ☐ |
| `icon:bell` | photoGrid/none, photoGrid/edit, photoGrid/quickDevelop +26 more | ✅ | ☐ |
| `icon:cloud` | photoGrid/none, photoGrid/edit, photoGrid/quickDevelop +26 more | ✅ | ☐ |
| `icon:compare` | photoGrid/none, photoGrid/edit, photoGrid/quickDevelop +26 more | ✅ | ☐ |
| `icon:copyGear` | photoGrid/none, detail/none, squareGrid/none +4 more | ✅ | ☐ |
| `icon:crop` | photoGrid/none, photoGrid/edit, photoGrid/quickDevelop +26 more | ✅ | ☐ |
| `icon:detail` | photoGrid/none, photoGrid/edit, photoGrid/quickDevelop +26 more | ✅ | ☐ |
| `icon:discord` | photoGrid/none, photoGrid/edit, photoGrid/quickDevelop +26 more | ✅ | ☐ |
| `icon:edit` | photoGrid/edit, photoGrid/crop, photoGrid/remove +15 more | ✅ | ☐ |
| `icon:editCaptureTime` | photoGrid/info, detail/info | ✅ | ☐ |
| `icon:faceBoxes` | detail/none, detail/edit, detail/quickDevelop +9 more | ✅ | ☐ |
| `icon:filmstrip` | detail/none, detail/edit, detail/quickDevelop +10 more | ✅ | ☐ |
| `icon:filter` | photoGrid/none, photoGrid/edit, photoGrid/quickDevelop +26 more | ✅ | ☐ |
| `icon:forward` | photoGrid/none, photoGrid/edit, photoGrid/quickDevelop +26 more | ✅ | ☐ |
| `icon:help` | photoGrid/none, photoGrid/edit, photoGrid/quickDevelop +26 more | ✅ | ☐ |
| `icon:home_library` | photoGrid/none, photoGrid/edit, photoGrid/quickDevelop +26 more | ✅ | ☐ |
| `icon:info` | photoGrid/none, photoGrid/edit, photoGrid/quickDevelop +26 more | ✅ | ☐ |
| `icon:infoRename` | photoGrid/info, detail/info | ✅ | ☐ |
| `icon:keywords` | photoGrid/none, photoGrid/edit, photoGrid/quickDevelop +26 more | ✅ | ☐ |
| `icon:maskOverlay` | photoGrid/masking, detail/masking | ✅ | ☐ |
| `icon:maskOverlayAll` | photoGrid/masking, detail/masking | ✅ | ☐ |
| `icon:masking` | photoGrid/none, photoGrid/edit, photoGrid/quickDevelop +26 more | ✅ | ☐ |
| `icon:more` | photoGrid/none, photoGrid/edit, photoGrid/quickDevelop +26 more | ✅ | ☐ |
| `icon:people` | photoGrid/none, photoGrid/edit, photoGrid/quickDevelop +26 more | ✅ | ☐ |
| `icon:photoGrid` | photoGrid/none, photoGrid/edit, photoGrid/quickDevelop +26 more | ✅ | ☐ |
| `icon:presets` | photoGrid/none, photoGrid/edit, photoGrid/quickDevelop +26 more | ✅ | ☐ |
| `icon:profileBrowser` | photoGrid/edit, detail/edit | ✅ | ☐ |
| `icon:profilesBack` | photoGrid/profiles, detail/profiles | ✅ | ☐ |
| `icon:quickDevelop` | photoGrid/none, photoGrid/quickDevelop, photoGrid/versions +8 more | ✅ | ☐ |
| `icon:redeye` | photoGrid/none, photoGrid/edit, photoGrid/quickDevelop +26 more | ✅ | ☐ |
| `icon:remove` | photoGrid/none, photoGrid/edit, photoGrid/quickDevelop +26 more | ✅ | ☐ |
| `icon:share` | photoGrid/none, photoGrid/edit, photoGrid/quickDevelop +26 more | ✅ | ☐ |
| `icon:sidebar` | photoGrid/none, photoGrid/edit, photoGrid/quickDevelop +26 more | ✅ | ☐ |
| `icon:sort` | photoGrid/none, photoGrid/edit, photoGrid/quickDevelop +26 more | ✅ | ☐ |
| `icon:squareGrid` | photoGrid/none, photoGrid/edit, photoGrid/quickDevelop +26 more | ✅ | ☐ |
| `icon:survey` | photoGrid/none, photoGrid/edit, photoGrid/quickDevelop +26 more | ✅ | ☐ |
| `icon:versions` | photoGrid/none, photoGrid/edit, photoGrid/quickDevelop +26 more | ✅ | ☐ |
