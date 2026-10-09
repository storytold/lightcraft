# Feature catalog

Every tracked feature id, grouped as in `docs/parity.md`, with its tier. The tracker holds the status and
evidence; this file is the stable list of *what* we track, in our own words. Ids never change once published.

Tiers: **P0** core (a photographer cannot switch without it) · **P1** important parity · **P2** later, niche or
AI-heavy · **OOS** out of scope (cloud-only or generative services we do not replicate).

Prefixes: `LR-` features shared by both Lightroom generations · `MENU-` menu items · `KEY-` desktop keys ·
`LRC-` Classic-only features · `KEYC-` Classic keys · `IMM-` Immich integration (beyond Lightroom).

## A. Import (IMP)

| Id | Feature | Tier |
|---|---|---|
| LR-IMP-ADD-DIALOG | Add photos/folders | P0 |
| LR-IMP-DRAGDROP | Drop files/folders to import | P0 |
| LR-IMP-DUPES | Skip duplicates by content | P1 |
| LR-IMP-DEVICE | Import from camera/card | P1 |
| LR-IMP-AUTO | Watched-folder auto import | P2 |
| LR-IMP-PRESET | Preset on import | P2 |
| LR-IMP-RAWDEFAULT | Raw defaults | P1 |
| LR-IMP-MIGRATE | Migrate other catalogs | OOS |
| LR-IMP-PROFILES | Import profiles & presets | P1 |
| LR-IMP-LOCAL | Work on files in place | P0 |
| LR-IMP-SIDECAR-SPLIT | Separate XMP sidecar variants | P2 |
| LR-IMP-FORMATS | Supported formats | P0 |
| LR-IMP-CAMERA-COVERAGE | Camera coverage, verified per model | P0 |
| LR-IMP-CULL-AT-IMPORT | Culling analysis at import | P2 |
| LR-IMP-MOVE | Move on import [Classic] | P1 |
| LR-IMP-DNG-CONVERT | Convert to DNG on import [Classic] | P2 |

## B. Library management (LIB)

| Id | Feature | Tier |
|---|---|---|
| LR-LIB-ALLPHOTOS | All photos | P0 |
| LR-LIB-RECENT-ADDED | Recently added | P1 |
| LR-LIB-BYDATE | Browse by date | P1 |
| LR-LIB-ALBUM | Albums | P0 |
| LR-LIB-FOLDER | Folders of albums | P0 |
| LR-LIB-SMARTALBUM | Smart albums | P1 |
| LR-LIB-SHARED-ALBUM | Shared albums | P2 |
| LR-LIB-OFFLINE | Keep album offline | P2 |
| LR-LIB-TARGET | Target album | P2 |
| LR-LIB-RATING | Star ratings | P0 |
| LR-LIB-FLAG | Pick / reject flags | P0 |
| LR-LIB-LABEL | Colour labels | P1 |
| LR-LIB-KEYWORD | Keywords | P0 |
| LR-LIB-PEOPLE | People / faces | P2 |
| LR-LIB-STACK | Stacks | P1 |
| LR-LIB-VERSIONS | Versions | P1 |
| LR-LIB-DELETE | Delete / Recently Deleted | P0 |
| LR-LIB-REMOVE-ALBUM | Remove from album | P0 |
| LR-LIB-DUPLICATE | Duplicate a photo | P2 |
| LR-LIB-RENAME | Batch rename | P1 |
| LR-LIB-CAPTURETIME | Edit capture time | P1 |
| LR-LIB-SHOWFINDER | Reveal original in file manager | P0 |
| LR-LIB-COVER | Album cover | P2 |
| LR-LIB-CULL | Assisted culling | P2 |
| LR-LIB-ACTIVITY | Comments & likes | OOS |
| LR-LIB-QUICKCOLL | Quick collection [Classic] | P2 |
| LR-LIB-VIRTUALCOPY | Virtual copies [Classic] | P1 |

## C. Views & navigation (VIEW)

| Id | Feature | Tier |
|---|---|---|
| LR-VIEW-PHOTOGRID | Justified photo grid | P0 |
| LR-VIEW-SQUAREGRID | Square grid | P0 |
| LR-VIEW-DETAIL | Single-photo view | P0 |
| LR-VIEW-EDIT | Edit view | P0 |
| LR-VIEW-FULLSCREEN | Full-screen preview | P1 |
| LR-VIEW-FILMSTRIP | Filmstrip | P0 |
| LR-VIEW-ZOOM | Zoom & pan | P0 |
| LR-VIEW-NAVIGATOR | Navigator mini map | P1 |
| LR-VIEW-BEFOREAFTER | Before / after | P0 |
| LR-VIEW-COMPARE | Compare two photos | P1 |
| LR-VIEW-SURVEY | Survey view [Classic] | P2 |
| LR-VIEW-INFOOVERLAY | Info overlay on the photo | P1 |
| LR-VIEW-SLIDESHOW | Slideshow | P2 |
| LR-VIEW-SECONDWINDOW | Second display window [Classic] | P2 |
| LR-VIEW-CLIPPING | Clipping indicators | P0 |
| LR-VIEW-HISTOGRAM | Histogram | P0 |
| LR-VIEW-HDR-DISPLAY | HDR display output | P2 |

## D. Search & filter (FILT)

| Id | Feature | Tier |
|---|---|---|
| LR-FILT-SEARCH-META | Text search | P0 |
| LR-FILT-SEARCH-AI | Natural-language search | P2 |
| LR-FILT-RATING | Rating filter | P0 |
| LR-FILT-FLAG | Flag filter | P0 |
| LR-FILT-LABEL | Colour-label filter | P1 |
| LR-FILT-TYPE | Type / edited filter | P1 |
| LR-FILT-KEYWORD | Keyword filter | P1 |
| LR-FILT-CAMERA | Camera / lens filter | P1 |
| LR-FILT-LOCATION | Location filter | P2 |
| LR-FILT-PEOPLE | People filter | P2 |
| LR-FILT-CULL | Culling-score filters | P2 |
| LR-FILT-SORT | Sort | P0 |
| LR-FILT-SAVED | Filter presets [Classic] | P2 |

## E. Metadata (META)

| Id | Feature | Tier |
|---|---|---|
| LR-META-INFO | Info panel | P0 |
| LR-META-COPYRIGHT-DEFAULT | Default copyright on import | P1 |
| LR-META-LOCATION | Location editing | P2 |
| LR-META-COPYPASTE | Copy / paste metadata | P2 |
| LR-META-XMP | XMP read/write | P0 |
| LR-META-EXIF-FULL | Full EXIF/IPTC [Classic] | P1 |

## F. Edit panel — global adjustments (EDIT)

| Id | Feature | Tier |
|---|---|---|
| LR-EDIT-AUTO | Auto settings | P0 |
| LR-EDIT-BW | Black & white | P0 |
| LR-EDIT-HDR-MODE | HDR editing | P2 |
| LR-EDIT-LIGHT-EXPOSURE | Exposure | P0 |
| LR-EDIT-LIGHT-CONTRAST | Contrast | P0 |
| LR-EDIT-LIGHT-HIGHLIGHTS | Highlights | P0 |
| LR-EDIT-LIGHT-SHADOWS | Shadows | P0 |
| LR-EDIT-LIGHT-WHITES | Whites | P0 |
| LR-EDIT-LIGHT-BLACKS | Blacks | P0 |
| LR-EDIT-LIGHT-CURVE-PARAM | Parametric curve | P0 |
| LR-EDIT-LIGHT-CURVE-POINT | Point curve | P0 |
| LR-EDIT-LIGHT-CURVE-RGB | Per-channel curves | P0 |
| LR-EDIT-LIGHT-CURVE-REFINESAT | Curve saturation compensation | P1 |
| LR-EDIT-LIGHT-CURVE-TAT | Drag-on-image curve adjust | P1 |
| LR-EDIT-COLOR-WB-PRESET | White-balance presets | P0 |
| LR-EDIT-COLOR-WB-PICKER | White-balance eyedropper | P0 |
| LR-EDIT-COLOR-TEMP | Temperature | P0 |
| LR-EDIT-COLOR-TINT | Tint | P0 |
| LR-EDIT-COLOR-VIBRANCE | Vibrance | P0 |
| LR-EDIT-COLOR-SATURATION | Saturation | P0 |
| LR-EDIT-COLOR-MIXER-HSL | 8-band colour mixer | P0 |
| LR-EDIT-COLOR-MIXER-BW | B&W mix | P1 |
| LR-EDIT-COLOR-POINTCOLOR | Point colour | P1 |
| LR-EDIT-COLOR-GRADING | Colour grading wheels | P0 |
| LR-EDIT-EFFECTS-TEXTURE | Texture | P0 |
| LR-EDIT-EFFECTS-CLARITY | Clarity | P0 |
| LR-EDIT-EFFECTS-DEHAZE | Dehaze | P0 |
| LR-EDIT-EFFECTS-VIGNETTE | Post-crop vignette | P0 |
| LR-EDIT-EFFECTS-GRAIN | Grain | P1 |
| LR-EDIT-DETAIL-SHARPEN | Sharpening | P0 |
| LR-EDIT-DETAIL-NR | Luminance noise reduction | P0 |
| LR-EDIT-DETAIL-CNR | Colour noise reduction | P0 |
| LR-EDIT-DETAIL-DENOISE | AI denoise | P2 |
| LR-EDIT-DETAIL-RAWDETAILS | Improved demosaic toggle | P2 |
| LR-EDIT-DETAIL-SUPERRES | Super resolution | P2 |
| LR-EDIT-DETAIL-AISHARPEN | AI sharpen | OOS |
| LR-EDIT-OPTICS-CA | Remove chromatic aberration | P1 |
| LR-EDIT-OPTICS-PROFILE | Lens profile corrections | P1 |
| LR-EDIT-OPTICS-DEFRINGE | Defringe | P1 |
| LR-EDIT-OPTICS-MANUAL | Manual distortion / vignetting | P1 |
| LR-EDIT-GEOM-UPRIGHT | Upright | P1 |
| LR-EDIT-GEOM-MANUAL | Manual transform | P1 |
| LR-EDIT-GEOM-CONSTRAIN | Constrain crop | P1 |
| LR-EDIT-GEOM-GRID | Grid while transforming | P2 |
| LR-EDIT-LENSBLUR | Lens blur | P2 |
| LR-EDIT-CALIB | Calibration [Classic] | P1 |
| LR-EDIT-SECTION-TOGGLE | Section on/off | P1 |
| LR-EDIT-RESET | Reset all / section / slider | P0 |
| LR-EDIT-SHOWORIG | Show original | P0 |

## G. Profiles (PROF)

| Id | Feature | Tier |
|---|---|---|
| LR-PROF-DROPDOWN | Profile menu | P0 |
| LR-PROF-BROWSER | Profile browser | P1 |
| LR-PROF-ADOBE | Standard raw looks (own equivalents) | P0 |
| LR-PROF-ADAPTIVE | Adaptive profiles | P2 |
| LR-PROF-CAMERA | Camera-matching looks | P2 |
| LR-PROF-CAMERACOLOR | Camera colour calibration (own) | P0 |
| LR-PROF-CREATIVE | Creative profiles (own) | P2 |
| LR-PROF-LEGACY | Legacy profiles | P2 |
| LR-PROF-NONRAW | Profiles for non-raw files | P0 |
| LR-PROF-AMOUNT | Profile amount | P1 |
| LR-PROF-IMPORT | Import profiles | P1 |

## H. Crop & rotate (CROP)

| Id | Feature | Tier |
|---|---|---|
| LR-CROP-RECT | Crop rectangle | P0 |
| LR-CROP-ASPECT | Aspect ratios | P0 |
| LR-CROP-STRAIGHTEN | Straighten tool | P0 |
| LR-CROP-AUTO | Auto straighten | P1 |
| LR-CROP-ANGLE | Angle slider | P0 |
| LR-CROP-ROTATE90 | Rotate 90° | P0 |
| LR-CROP-FLIP | Flip | P0 |
| LR-CROP-OVERLAY | Crop overlays | P1 |
| LR-CROP-ZOOM | Zoom while cropping | P1 |
| LR-CROP-GENEXPAND | Generative expand | OOS |

## I. Remove / healing (REM)

| Id | Feature | Tier |
|---|---|---|
| LR-REM-CONTENTAWARE | Content-aware remove | P1 |
| LR-REM-HEAL | Heal | P0 |
| LR-REM-CLONE | Clone | P0 |
| LR-REM-GEN | Generative remove | OOS |
| LR-REM-DETECT | Object detection for remove | P2 |
| LR-REM-BRUSH-PARAMS | Brush size / feather / opacity | P0 |
| LR-REM-SPOT-EDIT | Edit existing spots | P0 |
| LR-REM-VISUALIZE | Visualize spots | P1 |
| LR-REM-PEOPLE | Remove people (generative) | OOS |
| LR-REM-REFLECT | Remove reflections | P2 |
| LR-REM-DUST | Dust detection | P2 |
| LR-REM-SYNC | Sync spots | P1 |

## J. Red eye (EYE)

| Id | Feature | Tier |
|---|---|---|
| LR-EYE-RED | Red-eye correction | P1 |
| LR-EYE-PET | Pet eye | P2 |

## K. Masking (MASK)

| Id | Feature | Tier |
|---|---|---|
| LR-MASK-PANEL | Masks panel | P0 |
| LR-MASK-SUBJECT | Select subject | P2 |
| LR-MASK-SKY | Select sky | P2 |
| LR-MASK-BACKGROUND | Select background | P2 |
| LR-MASK-OBJECTS | Object selection | P2 |
| LR-MASK-PEOPLE | People parts | P2 |
| LR-MASK-LANDSCAPE | Landscape classes | P2 |
| LR-MASK-BRUSH | Brush mask | P0 |
| LR-MASK-LINEAR | Linear gradient | P0 |
| LR-MASK-RADIAL | Radial gradient | P0 |
| LR-MASK-COLORRANGE | Colour range | P1 |
| LR-MASK-LUMRANGE | Luminance range | P1 |
| LR-MASK-DEPTHRANGE | Depth range | P2 |
| LR-MASK-COMBINE | Add / subtract / intersect | P0 |
| LR-MASK-INVERT | Invert | P0 |
| LR-MASK-AMOUNT | Mask amount | P1 |
| LR-MASK-FEATHER-EDGE | Refine mask edges | P2 |
| LR-MASK-SLIDERS | Local adjustment sliders | P0 |
| LR-MASK-OVERLAY | Mask overlay | P0 |
| LR-MASK-PINS | Pins | P1 |
| LR-MASK-UPDATE | Recompute AI masks | P2 |
| LR-MASK-SYNC | Copy masks to other photos | P1 |
| LR-MASK-ADAPTIVE | Adaptive presets | P2 |

## L. Presets (PRE)

| Id | Feature | Tier |
|---|---|---|
| LR-PRE-PANEL | Presets panel | P0 |
| LR-PRE-CREATE | Create preset | P0 |
| LR-PRE-MANAGE | Manage presets | P1 |
| LR-PRE-AMOUNT | Preset amount | P1 |
| LR-PRE-ADAPTIVE | Adaptive presets | P2 |
| LR-PRE-PREMIUM | Built-in presets (own) | P2 |
| LR-PRE-RECOMMENDED | Community recommendations | OOS |
| LR-PRE-ONIMPORT | Apply during import | P2 |

## M. Versions & history (VER)

| Id | Feature | Tier |
|---|---|---|
| LR-VER-CREATE | Create version | P1 |
| LR-VER-PANEL | Versions panel | P1 |
| LR-VER-AUTO | Automatic versions | P2 |
| LR-VER-HISTORY | Edit history | P1 |
| LR-VER-UNDO | Undo / redo | P0 |

## N. Copy / paste / sync (SYNC)

| Id | Feature | Tier |
|---|---|---|
| LR-SYNC-COPY | Copy edit settings | P0 |
| LR-SYNC-CHOOSE | Choose settings to copy | P0 |
| LR-SYNC-PASTE | Paste to selection | P0 |
| LR-SYNC-SYNCBTN | Sync active → selected | P1 |
| LR-SYNC-PREVIOUS | Paste from previous / auto sync [Classic] | P2 |

## O. Merge (MERGE)

| Id | Feature | Tier |
|---|---|---|
| LR-MERGE-HDR | HDR merge | P2 |
| LR-MERGE-PANO | Panorama | P2 |
| LR-MERGE-HDRPANO | HDR panorama | P2 |
| LR-MERGE-HEADLESS | Merge with last settings | P2 |

## P. Enhance (ENH)

| Id | Feature | Tier |
|---|---|---|
| LR-ENH-DIALOG | Enhance dialog | P2 |
| LR-ENH-INPLACE | In-place enhance | P2 |

## Q. HDR (HDR)

| Id | Feature | Tier |
|---|---|---|
| LR-HDR-EDIT | HDR editing | P2 |
| LR-HDR-SDRPREVIEW | SDR preview of HDR | P2 |
| LR-HDR-VISUALIZE | Visualize HDR range | P2 |
| LR-HDR-LIMIT | HDR headroom limit | P2 |
| LR-HDR-EXPORT | HDR export | P2 |

## R. Video (VID)

| Id | Feature | Tier |
|---|---|---|
| LR-VID-PLAY | Video playback | P1 |
| LR-VID-TRIM | Trim | P1 |
| LR-VID-EDIT | Edits on video | P2 |
| LR-VID-COVER | Cover frame | P2 |
| LR-VID-EXPORT | Video export | P2 |
| LR-VID-PHOTO2VIDEO | Photo to video (generative) | OOS |

## S. Export (EXP)

| Id | Feature | Tier |
|---|---|---|
| LR-EXP-DIALOG | Export dialog | P0 |
| LR-EXP-TYPE | File types | P0 |
| LR-EXP-DIM | Output size | P0 |
| LR-EXP-QUALITY | JPEG quality | P0 |
| LR-EXP-BITDEPTH | Bit depth | P1 |
| LR-EXP-COMPRESSION | TIFF compression | P1 |
| LR-EXP-COLORSPACE | Output colour space | P0 |
| LR-EXP-HDR | HDR output | P2 |
| LR-EXP-SHARPEN | Output sharpening | P1 |
| LR-EXP-METADATA | Metadata policy | P1 |
| LR-EXP-WATERMARK | Watermark | P1 |
| LR-EXP-NAMING | File naming | P1 |
| LR-EXP-LOCATION | Destination folder | P0 |
| LR-EXP-PREVIOUS | Export with previous settings | P0 |
| LR-EXP-DNGOPT | DNG options | P2 |
| LR-EXP-ORIGINAL | Original + XMP | P1 |
| LR-EXP-PHOTOS | Export to the system photo library | P2 |
| LR-EXP-PSD | Round trip to an external editor | P2 |

## T. Share (SHARE)

| Id | Feature | Tier |
|---|---|---|
| LR-SHARE-LINK | Web share link | OOS |
| LR-SHARE-INVITE | Invite collaborators | OOS |
| LR-SHARE-WEBGALLERY | Web galleries | OOS |
| LR-SHARE-COMMUNITY | Community edits | OOS |

## U. Map & location (MAP)

| Id | Feature | Tier |
|---|---|---|
| LR-MAP-INFO | Location in the info panel | P1 |
| LR-MAP-MODULE | Map module [Classic] | P2 |

## V. Preferences (PREF)

| Id | Feature | Tier |
|---|---|---|
| LR-PREF-GENERAL | General settings | P0 |
| LR-PREF-LOCALSTORAGE | Storage & cache | P1 |
| LR-PREF-ACCOUNT | Account | OOS |
| LR-PREF-INTERFACE | Interface options | P1 |
| LR-PREF-PERFORMANCE | GPU / performance | P1 |
| LR-PREF-PEOPLE | Face recognition | P2 |
| LR-PREF-WATERMARK | Watermark settings | P1 |
| LR-PREF-SHORTCUTS | Shortcut customisation | — |
| LR-PREF-TECHPREVIEW | Early-access toggles | P2 |
| LR-PREF-NOTIFICATIONS | Notifications | OOS |
| LR-PREF-DEVICE | Device settings | P2 |

## W. Cloud & AI infrastructure (CLOUD / AI)

| Id | Feature | Tier |
|---|---|---|
| LR-CLOUD-SYNC | Cloud sync | OOS |
| LR-CLOUD-SMARTPREVIEW | Editable proxies | P2 |
| LR-AI-UPDATE-INDICATOR | AI-settings update indicator | P2 |
| LR-AI-CREDITS | Generative credits | OOS |

## X. Cross-cutting behaviours (BEHAV)

| Id | Feature | Tier |
|---|---|---|
| LR-BEHAV-AUTOSAVE | Instant autosave | P0 |
| LR-BEHAV-UNDO | Global undo | P0 |
| LR-BEHAV-MULTISELECT | Multi-selection | P0 |
| LR-BEHAV-BATCH | Batch apply to selection | P0 |
| LR-BEHAV-PREVIEW-HOVER | Hover previews | P1 |
| LR-BEHAV-PROGRESSIVE | Progressive rendering | P0 |
| LR-BEHAV-BG-TASKS | Background tasks | P0 |
| LR-BEHAV-OFFLINE | Offline editing | P1 |
| LR-BEHAV-WEB-SAFETY | Library safety in the browser build | P1 |
| LR-BEHAV-GPU | GPU acceleration | P0 |
| LR-BEHAV-RENDER-FIDELITY | Rendering matches Lightroom | P1 |
| LR-BEHAV-DRAGDROP | Drag and drop | P1 |
| LR-BEHAV-TOAST | Toast notifications | P1 |
| LR-BEHAV-SIDEBAR-COLLAPSE | Collapsible left-sidebar sections | P2 |
| LR-BEHAV-PANEL-RESIZE | Resizable side panels | P1 |
| LR-BEHAV-EMPTY-STATES | Empty states | P1 |
| LR-BEHAV-TOOLTIPS | Tooltips with shortcuts | P0 |
| LR-BEHAV-HEADLESS | Headless UI snapshots | P2 |
| LR-BEHAV-ACCESS | Accessibility | P2 |
| LR-BEHAV-LOCALE | Language options | P2 |
| LR-BEHAV-LOCALIZE | Localisation | P2 |
| LR-BEHAV-LEARN | Tutorials | OOS |
| LR-BEHAV-WHATSNEW | What's new | P2 |
| LR-BEHAV-AI-EA | Early-access badges | P2 |
| LR-BEHAV-TITLEBAR | Title bar drag and double-click zoom (macOS) | P2 |

## Y. Menus

| Id | Feature | Tier |
|---|---|---|
| MENU-BAR | Menu bar rendering | P1 |
| MENU-APP-ABOUT | About | P2 |
| MENU-APP-SETTINGS | Settings… | P0 |
| MENU-APP-UPDATES | Check for updates | P2 |
| MENU-APP-SYNC | Sync status / pause | OOS |
| MENU-APP-SIGNOUT | Sign out | OOS |
| MENU-APP-HIDE | Hide / hide others / show all | P1 |
| MENU-APP-QUIT | Quit | P0 |
| MENU-FILE-ADDPHOTOS | Import Photos… (was Add Photos…) | P0 |
| MENU-FILE-ADDFOLDER | Import from Folder… (was Add Folder…) | P0 |
| MENU-FILE-MIGRATE | Migrate photos | OOS |
| MENU-FILE-NEWALBUM | New Album… | P0 |
| MENU-FILE-NEWFOLDER | New Folder… | P0 |
| MENU-FILE-NEWSMART | New Smart Album… | P1 |
| MENU-FILE-IMPORTPROFILES | Import Profiles & Presets… | P1 |
| MENU-FILE-EXPORT | Export… | P0 |
| MENU-FILE-EXPORTPREV | Export with Previous | P0 |
| MENU-FILE-EXPORTPRESETS | Export preset submenu | P0 |
| MENU-FILE-SHARE | Share / get link / invite | OOS |
| MENU-FILE-PHOTOSHOP | Edit in external editor | P2 |
| MENU-FILE-SHOWFINDER | Show in Finder | P0 |
| MENU-FILE-OFFLINE | Store album locally | P2 |
| MENU-FILE-CLOSE | Close Window | P1 |
| MENU-EDIT-UNDO | Undo | P0 |
| MENU-EDIT-REDO | Redo | P0 |
| MENU-EDIT-COPYPASTE | Copy / paste (edit settings) | P0 |
| MENU-EDIT-CHOOSECOPY | Choose Edit Settings to Copy… | P0 |
| MENU-EDIT-PASTESELECTED | Paste Selected Settings | P0 |
| MENU-EDIT-SELECTALL | Select All | P0 |
| MENU-EDIT-SELECTNONE | Select None | P0 |
| MENU-EDIT-SELECTBY | Select by flag / rating | P1 |
| MENU-EDIT-FIND | Find… | P0 |
| MENU-VIEW-PHOTOGRID | Photo Grid | P0 |
| MENU-VIEW-SQUAREGRID | Square Grid | P0 |
| MENU-VIEW-DETAIL | Detail | P0 |
| MENU-VIEW-EDIT | Edit | P0 |
| MENU-VIEW-FULLSCREENPREVIEW | Full Screen Preview | P1 |
| MENU-VIEW-ENTERFULLSCREEN | Enter Full Screen | P1 |
| MENU-VIEW-PHOTOSPANEL | Show/Hide photos panel | P0 |
| MENU-VIEW-FILMSTRIP | Show/Hide filmstrip | P0 |
| MENU-VIEW-INFO | Show/Hide info | P0 |
| MENU-VIEW-KEYWORDS | Show/Hide keywords | P0 |
| MENU-VIEW-ACTIVITY | Show/Hide activity (comments) | OOS |
| MENU-VIEW-VERSIONS | Show/Hide versions | P1 |
| MENU-VIEW-HISTOGRAM | Show/Hide histogram | P0 |
| MENU-VIEW-INFOOVERLAY | Show info overlay | P1 |
| MENU-VIEW-SHOWORIGINAL | Show Original | P0 |
| MENU-VIEW-BEFOREAFTER | Before/After submenu | P0 |
| MENU-VIEW-ZOOM | Zoom in / out / toggle / fit / 1:1 | P0 |
| MENU-VIEW-CLIPPING | Show Clipping | P0 |
| MENU-VIEW-MASKOVERLAY | Mask overlay / cycle colour | P0 |
| MENU-VIEW-INCLUDESUBFOLDERS | Include subfolders | P1 |
| MENU-VIEW-SORT | Sort submenu | P0 |
| MENU-VIEW-STACKS | Expand/collapse stacks | P1 |
| MENU-VIEW-PHOTOCOUNT | Show photo counts | P2 |
| MENU-VIEW-HDR | HDR display options | P2 |
| MENU-PHOTO-ADDTOALBUM | Add to album | P0 |
| MENU-PHOTO-REMOVEFROMALBUM | Remove from album | P0 |
| MENU-PHOTO-RATE | Rate submenu | P0 |
| MENU-PHOTO-FLAG | Flag submenu | P0 |
| MENU-PHOTO-LABEL | Colour label submenu | P1 |
| MENU-PHOTO-ROTATE | Rotate left / right | P0 |
| MENU-PHOTO-FLIP | Flip horizontal / vertical | P0 |
| MENU-PHOTO-CREATEVERSION | Create Version… | P1 |
| MENU-PHOTO-STACK | Stack submenu | P1 |
| MENU-PHOTO-MERGE | Photo merge submenu | P2 |
| MENU-PHOTO-ENHANCE | Enhance… | P2 |
| MENU-PHOTO-AUTO | Auto settings | P0 |
| MENU-PHOTO-BW | Convert to B&W | P0 |
| MENU-PHOTO-RESET | Reset edits / crop | P0 |
| MENU-PHOTO-UPDATEAI | Update AI settings | P2 |
| MENU-PHOTO-RENAME | Rename N photos… | P1 |
| MENU-PHOTO-CAPTURETIME | Edit capture time… | P1 |
| MENU-PHOTO-COVER | Set as album cover | P2 |
| MENU-PHOTO-DELETE | Delete N photos… | P0 |
| MENU-PHOTO-MOVETOCLOUD | Move/copy to cloud | OOS |
| MENU-WINDOW-MINIMIZE | Minimize / zoom | P1 |
| MENU-WINDOW-PANELS | Panel switches | P0 |
| MENU-WINDOW-BRINGFRONT | Bring all to front | P2 |
| MENU-HELP-HELP | Help | P2 |
| MENU-HELP-TUTORIALS | Tutorials | OOS |
| MENU-HELP-WHATSNEW | What's new | P2 |
| MENU-HELP-SHORTCUTS | Keyboard shortcuts | P1 |
| MENU-HELP-FEEDBACK | Send feedback | P2 |
| MENU-HELP-LOGFOLDER | Open log folder | P2 |
| MENU-HELP-SYSINFO | System info | P2 |
| MENU-CTX-GRID | Photo context menu | P0 |
| MENU-CTX-DETAIL | Loupe context menu | P1 |
| MENU-CTX-ALBUM | Album / folder row menu | P0 |
| MENU-CTX-MASK | Mask / component menu | P0 |
| MENU-CTX-PRESET | Preset menu | P1 |
| MENU-CTX-PROFILE | Profile favourites | P2 |
| MENU-CTX-VERSION | Version menu | P1 |
| MENU-CTX-KEYWORD | Keyword chip menu | P1 |

## Z. Keyboard shortcuts (desktop)

| Id | Feature | Tier |
|---|---|---|
| KEY-CROP | Crop & rotate — C | P0 |
| KEY-DETAIL | Detail — D | P0 |
| KEY-EDIT | Edit — E | P0 |
| KEY-FULLSCREEN | Full-screen preview — F | P1 |
| KEY-GRID | Grid — G | P0 |
| KEY-INFO | Info — I | P0 |
| KEY-KEYWORDS | Keywords — K | P0 |
| KEY-CLIPBOARD | Copy / paste edit settings — ⌘C / ⌘V | P0 |
| KEY-UNDOREDO | Undo / redo — ⌘Z / ⇧⌘Z | P0 |
| KEY-MINIMIZE | Minimize — ⌘M | P1 |
| KEY-AUTO | Auto — ⇧A | P0 |
| KEY-PHOTOSHOP | External editor — ⇧⌘E | P2 |
| KEY-ROTATE | Rotate — ⌘[ / ⌘] | P0 |
| KEY-ZOOM | Zoom in / out — ⌘= / ⌘− | P0 |
| KEY-SELECTALL | Select all — ⌘A | P0 |
| KEY-SELECTNONE | Select none — ⌘D | P0 |
| KEY-PASTESELECTED | Paste selected — ⇧⌘V | P0 |
| KEY-PREFS | Settings — ⌘, | P0 |
| KEY-SEARCH | Search — ⌘F | P0 |
| KEY-VISUALIZESPOTS | Visualize spots — A | P1 |
| KEY-CYCLEOVERLAY | Cycle overlay — O | P0 |
| KEY-PHOTOSPANEL | Photos panel — P | P0 |
| KEY-LINEAR | Linear gradient — L | P0 |
| KEY-RADIAL | Radial gradient — R | P0 |
| KEY-CLIPPING | Clipping — J | P0 |
| KEY-WB | White-balance selector — W | P0 |
| KEY-FILMSTRIP | Filmstrip — / | P0 |
| KEY-SHOWORIGINAL | Show original — \ | P0 |
| KEY-TOGGLEZOOM | Toggle zoom — Space | P0 |
| KEY-MASKCOLOR | Cycle mask colour — ⇧O | P1 |
| KEY-EXPORTPREV | Export with previous — ⌘E | P0 |
| KEY-EXPORTDIALOG | Export dialog — ⇧E | P0 |
| KEY-ENTERFULLSCREEN | Window full screen — ⇧⌘F | P1 |
| KEY-STACK | Group / ungroup stack — ⌘G / ⇧⌘G | P1 |
| KEY-GUIDEDUPRIGHT | Guided Upright — ⇧G | P1 |
| KEY-HIDE | Hide / hide others — ⌘H / ⌥⌘H | P1 |
| KEY-QUIT | Quit — ⌘Q | P0 |
| KEY-CREATEVERSION | Create version — ⇧M | P1 |
| KEY-CLOSEWINDOW | Close window — ⌘W | P1 |
| KEY-DELETE | Delete photo — ⌫ | P0 |
| KEY-ADDPHOTOS | Import photos — ⇧⌘I | P0 |
| KEY-VERSIONS | Versions panel — ⇧V | P1 |
| KEY-SECTIONS | Expand/collapse edit sections — ⌘1…⌘6 | P1 |
| KEY-PRESETS | Presets panel — ⇧P | P0 |
| KEY-HISTOGRAM | Histogram — ⌘0 | P0 |
| KEY-BRUSHSIZE | Brush size — `[` / `]` | P0 |
| KEY-BRUSHFEATHER | Brush feather — ⇧`[` / ⇧`]` | P0 |
| KEY-BRUSH | Brush — B | P0 |
| KEY-HEAL | Remove / heal — H | P0 |
| KEY-MERGE | HDR / panorama merges — ⌃H ⇧⌃H ⌃M ⇧⌃M | P2 |
| KEY-PICK | Pick — Z | P0 |
| KEY-UNFLAG | Unflag — U | P0 |
| KEY-REJECT | Reject — X | P0 |
| KEY-RATING | Ratings — 0…5 | P0 |
| KEY-LABELS | Labels — 6…9 | P1 |
| KEY-MASKING | Masking — M | P0 |
| KEY-ERASE | Erase while held — ⌥ | P0 |
| KEY-RATEADVANCE | Rate and advance — ⇧0…5 | P1 |
| KEY-FLAGADVANCE | Flag and advance — ⇧Z / ⇧X / ⇧U | P1 |
| KEY-NEXTPREV | Next / previous — → / ← | P0 |
| KEY-BA-CYCLE | Before/after — Y | P0 |
| KEY-BA-TOPBOTTOM | Before/after top/bottom — ⌥Y | P1 |
| KEY-BA-SPLIT | Split before/after — ⇧Y | P0 |
| KEY-CHOOSECOPY | Choose settings to copy — ⇧⌘C | P0 |
| KEY-RESETALL | Reset all — ⇧⌘R | P0 |
| KEY-GENAI | Generative toggles / variations — ⌥⇧G, ⌥←/→ | OOS |
| KEY-DETECT | Detect objects — ⌥⇧O | P2 |
| KEY-CROP-CONSTRAIN | Lock crop aspect — A | P1 |
| KEY-CROP-SWAP | Swap crop orientation — X | P0 |
| KEY-CROP-OVERLAYORIENT | Crop overlay orientation — ⇧O | P1 |
| KEY-CROP-RESET | Reset crop — ⌥⌘R | P0 |
| KEY-STRAIGHTEN | Straighten while held — ⌘ drag | P1 |
| KEY-SLIDER-RESET | Reset slider — double-click | P0 |
| KEY-SLIDER-NUDGE | Nudge slider — ↑/↓ | P1 |
| KEY-SLIDER-TYPE | Type a slider value | P1 |
| KEY-SHORTCUTS | Shortcut list — ⌘/ | P1 |
| KEY-HELP | Help — F1 | P2 |
| KEY-VIDEO-PLAY | Play/pause video — Space | P1 |
| KEY-ESC | Leave tool / view — Esc | P0 |
| KEY-COMMIT | Commit tool — Return | P1 |
| KEY-DELETE-PIN | Delete selected pin — ⌫ | P0 |
| KEY-HIDEPINS | Hide pins — H | P2 |

## Lightroom Classic extras

| Id | Feature | Tier |
|---|---|---|
| LRC-LIB-IMPORT | Full import dialog | P1 |
| LRC-LIB-AUTOIMPORT | Watched-folder import | P2 |
| LRC-LIB-TETHER | Tethered capture | P2 |
| LRC-LIB-VIEWS | Grid / loupe / compare / survey / people | P1 |
| LRC-LIB-COMPARE | Compare view | P1 |
| LRC-LIB-SURVEY | Survey view | P2 |
| LRC-LIB-REFVIEW | Reference view | P2 |
| LRC-LIB-CATALOG-PANEL | Catalog sets | P1 |
| LRC-LIB-CATALOG-IMPORT | Native Lightroom Classic catalog import | P1 |
| LRC-LIB-CATALOG-LOCK | Catalog open in one program at a time | P1 |
| LRC-LIB-FOLDERS | Disk folder tree | P1 |
| LRC-LIB-LIBFOLDERS | Library folders | P1 |
| LRC-LIB-COLLECTIONS | Collections & sets | P1 |
| LRC-LIB-SMARTCOLL | Smart-collection rules | P1 |
| LRC-LIB-PUBLISH | Publish services | P2 |
| LRC-LIB-FILTERBAR | Library filter bar | P1 |
| LRC-LIB-STACKS | Stacks (full) | P1 |
| LRC-LIB-VC | Virtual copies | P1 |
| LRC-LIB-LABELS | Colour-label sets | P1 |
| LRC-LIB-KEYWORDS | Hierarchical keywords, sets, painter | P1 |
| LRC-LIB-METADATA | Metadata panel & presets | P1 |
| LRC-LIB-QUICKDEV | Quick develop | P2 |
| LRC-LIB-PEOPLE | People view | P2 |
| LRC-LIB-COMMENTS | Comments panel | P2 |
| LRC-LIB-VISUALSEARCH | Find similar photos | P2 |
| LRC-LIB-MISSING | Missing files & relink | P1 |
| LRC-LIB-CONVERT | Convert to DNG | P2 |
| LRC-LIB-PREVIEWS | Build / discard previews | P1 |
| LRC-LIB-SLIDESHOW-IMPROMPTU | Impromptu slideshow | P2 |
| LRC-DEV-SNAPSHOTS | Named snapshots | P1 |
| LRC-DEV-HISTORY | Full history panel | P1 |
| LRC-DEV-SOFTPROOF | Soft proofing | P2 |
| LRC-DEV-AUTOSYNC | Sync / auto sync / paste previous | P1 |
| LRC-DEV-MATCHEXP | Match total exposures | P2 |
| LRC-DEV-CALIB | Calibration panel | P1 |
| LRC-DEV-TAT | Targeted adjustment tools | P1 |
| LRC-DEV-DEFAULTS | Per-camera raw defaults | P1 |
| LRC-DEV-VIEWOPTIONS | Develop view options | P2 |
| LRC-DEV-VIDEO | Video frame capture | P2 |
| LRC-MAP-VIEW | Map view | P2 |
| LRC-MAP-GEOTAG | Drag photos onto the map | P2 |
| LRC-MAP-LOCATIONS | Saved locations | P2 |
| LRC-MAP-TRACKLOG | GPS track logs | P2 |
| LRC-MAP-FILTER | Location filter bar | P2 |
| LRC-MAP-REVGEO | Reverse geocoding | P2 |
| LRC-BOOK-SETTINGS | Book settings | P2 |
| LRC-BOOK-AUTOLAYOUT | Book auto layout | P2 |
| LRC-BOOK-PAGE | Book pages & templates | P2 |
| LRC-BOOK-GUIDES | Book guides | P2 |
| LRC-BOOK-CELL | Book cell padding | P2 |
| LRC-BOOK-TEXT | Book photo/page text | P2 |
| LRC-BOOK-TYPE | Book typography | P2 |
| LRC-BOOK-BG | Book backgrounds | P2 |
| LRC-BOOK-VIEWS | Book views | P2 |
| LRC-BOOK-EXPORT | Book export (PDF/JPEG) | P2 |
| LRC-SS-TEMPLATES | Slideshow templates | P2 |
| LRC-SS-OPTIONS | Slideshow options | P2 |
| LRC-SS-LAYOUT | Slideshow layout | P2 |
| LRC-SS-OVERLAYS | Slideshow overlays | P2 |
| LRC-SS-BACKDROP | Slideshow backdrop | P2 |
| LRC-SS-TITLES | Slideshow titles | P2 |
| LRC-SS-MUSIC | Slideshow music | P2 |
| LRC-SS-PLAYBACK | Slideshow playback | P2 |
| LRC-SS-EXPORT | Slideshow export | P2 |
| LRC-PRINT-LAYOUTSTYLE | Print layout styles | P2 |
| LRC-PRINT-IMAGESETTINGS | Print image settings | P2 |
| LRC-PRINT-LAYOUT | Print layout | P2 |
| LRC-PRINT-GUIDES | Print guides | P2 |
| LRC-PRINT-CELLS | Picture-package cells | P2 |
| LRC-PRINT-PAGE | Print page options | P2 |
| LRC-PRINT-JOB | Print job & colour management | P2 |
| LRC-PRINT-TEMPLATES | Print templates | P2 |
| LRC-WEB-LAYOUT | Web gallery layouts | P2 |
| LRC-WEB-SITEINFO | Web gallery site info | P2 |
| LRC-WEB-COLOR | Web gallery colours | P2 |
| LRC-WEB-APPEARANCE | Web gallery appearance | P2 |
| LRC-WEB-IMAGEINFO | Web gallery image info | P2 |
| LRC-WEB-OUTPUT | Web gallery output | P2 |
| LRC-WEB-UPLOAD | Web gallery upload | P2 |
| LRC-SHELL-MODULES | Module picker (Library, Develop, Map, Book, Slideshow, Print, Web), hide modules | P1 |
| LRC-SHELL-PANELS | Panel system: four sides, auto hide/show, solo mode, toolbar toggle | P1 |
| LRC-SHELL-SCREENMODES | Screen modes and lights out (dim / off) | P2 |
| LRC-SHELL-SECONDWINDOW-MODES | Secondary display: live / locked loupe, grid, compare, survey, slideshow | P2 |
| LRC-SHELL-KEYMAP | Classic keymap layer (switchable) | P1 |
| LRC-SHELL-IDPLATE | Identity plate (styled text or graphic) and activity centre | P2 |
| LRC-SHELL-PREFS | Classic preference groups (presets, external editing, file handling, display, network) | P2 |
| LRC-SHELL-PLUGINS | Plug-in manager and SDK (sandboxed) | P2 |
| LRC-CAT-SCALE | Catalog of 500k+ photos with an on-disk index | P1 |
| LRC-CAT-MULTI | Several catalogs: create, open, open recent, choose at startup | P1 |
| LRC-CAT-BACKUP | Catalog backup on exit with integrity test and optimise | P1 |
| LRC-CAT-EXPORT | Export as catalog (subset, with or without originals and previews) and import from another catalog | P1 |
| LRC-CAT-SETTINGS | Catalog settings: backup schedule, preview size/quality, discard 1:1 previews, auto-write XMP, address lookup, face detection | P1 |
| LRC-CAT-XMPCONFLICT | Metadata changed on disk: badge, read from / save to file | P1 |
| LRC-IMP-SECONDCOPY | Make a second copy (backup) during import | P2 |
| LRC-IMP-PRESETS | Import presets (saved dialog settings) | P2 |
| LRC-LIB-KEYWORDLIST | Keyword list: synonyms, export flags, import / export keyword lists | P2 |
| LRC-LIB-PROCESSVERSION | Find previous process version, update DNG previews | P2 |
| LRC-LIB-LAYERS | Open as layers in an external editor (layered round trip) | P2 |
| LRC-DEV-HISTOGRAM-DRAG | Drag on the histogram to adjust tone regions | P2 |
| LRC-MAP-PINS | Map pins, clusters and hover previews | P2 |
| LRC-BOOK-SAVED | Saved books | P2 |
| LRC-SS-SAVED | Saved slideshows | P2 |
| LRC-PRINT-PAGESETUP | Page setup, printer settings, print one copy | P2 |
| LRC-WEB-SAVED | Saved web galleries | P2 |
| LRC-EXP-EMAIL | Email photos | P2 |
| LRC-EXP-POSTPROCESS | Post-processing after export (open in app, export actions) | P2 |
| LRC-AUTO-ACTIONS | Recordable actions / batch scripts | P2 |
| LRC-VID-FRAME | Capture video frame as a still | P2 |
| KEYC-PANELS | Classic panel keys (Tab, ⇧Tab, T, F5–F8, solo) | P2 |
| KEYC-MODULES | Classic module switching (⌘⌥1–7) | P1 |
| KEYC-VIEWS | Classic view keys (E, G, C, N, L, F, I, ⇧R, ⌘⌥0) | P2 |
| KEYC-SECONDWINDOW | Classic secondary-window keys | P2 |
| KEYC-CATALOG | Classic photo/catalog keys (⇧⌘I, ⌘', ⌘R, F2, ⌫, ⇧⌘E…) | P2 |
| KEYC-COMPARE | Classic grid/compare keys (Z, Home/End, =/−, ⌘⇧D, S…) | P2 |
| KEYC-RATING | Classic rating/flag keys (1–5, ⇧1–5, 6–9, P, X, U, ⇧X, ⇧U, `[` `]`, \`) | P2 |
| KEYC-COLLECTIONS | Classic collection keys (⌘N, B…) | P2 |
| KEYC-METADATA | Classic keyword/metadata keys (⌘K, ⌘S, ⌘⌥⇧C/V…) | P2 |
| KEYC-DEVELOP | Classic develop keys (V, ⌘U, ⇧⌘U, R, Q, K, M, ⇧M, ⇧W, ⇧J, ⇧Q…) | P2 |
| KEYC-MODULE-OUTPUT | Book / slideshow / print / map / web keys | P2 |
| KEYC-HELP | Classic help keys (⌘/, F1) | P2 |

## IMM. Immich integration

| Id | Feature | Tier |
|---|---|---|
| IMM-CONNECT | Connect to one or more Immich servers (URL + API key), version check, secure key storage | P1 |
| IMM-LINK | Link catalog photos to Immich assets by checksum; "in Immich" badge and filter | P1 |
| IMM-IMPORT | Immich as an import source (browse albums/timeline, download originals or add as linked) | P1 |
| IMM-EXTLIB | Shared-originals mode: Immich external library and catalog over the same folders, no duplicate uploads | P1 |
| IMM-SHARELINK | Create Immich shared links for published albums (from Web/Slideshow/Publish) | P2 |
| IMM-PUBLISH | Immich publish service: collections → albums, renders and/or originals, re-publish, stacks | P1 |
| IMM-SYNC | Two-way metadata sync (rating, favourite, title/description, tags ↔ keywords, albums ↔ collections, GPS, time, archive) | P1 |
| IMM-PEOPLE | Import Immich people and face regions into People; push names back | P2 |
| IMM-SEARCH | Immich smart (CLIP) and metadata search from the Library filter bar | P2 |
| IMM-MCP | Immich commands available via CLI, control channel and MCP | P2 |
