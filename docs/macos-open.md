# Opening photos from macOS

Finder's Open With, opening a file whose chosen default is LightCraft, and dropping files on the
Dock icon add those files to the current library in place. The original stays where it is.
LightCraft selects the imported photo and shows Detail. Opening an existing photo selects it
without creating a duplicate. The normal import rules apply to sidecars, raw defaults,
Recently Deleted, unreadable files and unsupported formats.

Opening a document uses the same add mode as dropping a photo on the window or passing a file on
the command line. Use File > Import Photos to choose Copy or Move. This is an import into My Photos;
Local > Browse Folder remains the way to browse without adding photos to My Photos (LR-IMP-LOCAL).

Requests received while starting, importing, scanning, synchronizing or showing an Import Photos
dialog (including its file picker) wait in order. Each request is bound to the current library
opening when received. Open Library, Open Recent Library and New Library refuse to switch while
requests or imports are outstanding and explain why. Each running import also checks its library
identity before starting its worker or committing its results.

Startup recovery is the exception: a failed library must be reopened, replaced, or explicitly
continued without saving before requests acquire their destination and the queue drains. This
choice remains available during recovery. Queued opens appear as "Waiting to import opened files"
in the activity stack. Cancel a row to discard that request, even while Import Photos or startup
recovery is open. The queue lasts for this process only. Quitting warns how many opened files have
not been imported and offers Quit and Cancel. Cancel keeps the queue and the current dialog choices.

Cancelling a running native Add import immediately releases its library-switch guard. A blocked
read may finish in the background, but its buffered and late results are discarded. Photos already
added stay in the original library, with one undo step. Close Import Photos and cancel any other
outstanding requests before switching libraries.

If Import Photos is confirmed while another import is active, confirmation is rejected with a
message. The dialog keeps the chosen files and options. Wait for the active import to finish,
then click Import again. A second confirmation never replaces or cancels the active worker.

## Bundle registration

`packaging/macos/package.sh`, called by `.github/workflows/release.yml`, assembles and signs the
app. It substitutes version fields in `Info.plist.in`, then runs `document_types.py` before signing.
The generator reads `lightcraft_engine::import::EXTENSIONS`, registers each supported extension
and references macOS's existing Uniform Type Identifiers (UTIs). The declaration policy targets the
supported macOS releases, 14, 15 and 26. CoreTypes and RawCameraTypes supply 34 of the 37 referenced
identifiers, including AVIF, JPEG XL, Sony SR2/SRF and Photoshop PSB. LightCraft does not redeclare these system types. The
bundle's older binary deployment target does not expand this declaration compatibility policy.
Every document type has role Editor and rank Alternate.

Only these imported declarations remain:

| Identifier | Filename tag | Conformance | Reason |
| --- | --- | --- | --- |
| `ai.storyteller.lightcraft.image.x3f` | `x3f` | `public.data`, `public.camera-raw-image` | No system declaration for Sigma X3F |
| `ai.storyteller.lightcraft.image.kdc` | `kdc` | `public.data`, `public.camera-raw-image` | No system declaration for Kodak KDC |
| `ai.storyteller.lightcraft.image.mef` | `mef` | `public.data`, `public.camera-raw-image` | Fallback for the MEF format added on main |

AVIF uses `public.avif`. The system declaration conforms to `public.heif-standard` and supplies the
`avif` extension, `image/avif` MIME type and `AVIF` OSType. JPEG XL uses `public.jpeg-xl`, conforming
to `public.image`, with the `jxl` extension and `image/jxl` MIME type. Neither needs a compatibility
declaration for the supported releases. The SDK's `UTTypeJPEGXL` constant is available from macOS
15.2; the string identifier and system image support predate that constant. Apple's
[JPEG XL identifier documentation](https://developer.apple.com/documentation/uniformtypeidentifiers/uttypejpegxl),
[Safari 17 format announcement](https://webkit.org/blog/14205/news-from-wwdc23-webkit-features-in-safari-17-beta/)
and [AVIF system-framework support record](https://bugs.webkit.org/show_bug.cgi?id=241904) describe
these identifiers and format support. The native CoreTypes regression checks the declaration
parents and tags directly on the machine running the tests.

If a future supported extension needs a compatibility declaration, copy the canonical system
identifier, conformance and all tags, including MIME and OSType tags when present. Add an exact
regression for those fields. Do not replace a system type with a generic image declaration.

The app does not explicitly set a default handler. LaunchServices can nevertheless choose an
Alternate handler as the default when no higher-ranked handler exists for that type. This is
acceptable for LightCraft; Alternate does not guarantee that the app can never become a default.

When changing EXTENSIONS, refresh the template for review and the plist-sync test:

```sh
python3 packaging/macos/document_types.py packaging/macos/Info.plist.in
cargo test -p lightcraft macos_document_types
```

The test compares the template with the compiled extension list and checks ranks, roles, system
UTI references and the exact fallback declarations and their extension tags. The macOS-only
`macos_coretypes_supplies_avif_and_jpeg_xl` test reads CoreTypes directly; run it on each supported
macOS version when checking the policy. HEIC/HEIF registration does not enable their optional
decoder; builds without it report the normal import error. AVIF likewise retains the existing decoder limitation.

## Event handling

The pinned eframe 0.37 / winit 0.30 stack does not forward document opens. `lightcraft-macos-open`
registers the Open Documents (`aevt/odoc`) Apple event after creating NSApplication and before
starting eframe. A notification observer registers it again at `NSApplicationWillFinishLaunching`,
after AppKit installs its handlers and before cold-start documents arrive. This is the event AppKit
normally translates to `application:openURLs:` or `application:openFiles:`. The bridge leaves
winit's application delegate and quit handling intact.
It holds its Objective-C handler until shutdown and removes its registration when dropped.

Foundation converts file descriptors to file URLs, including legacy alias descriptors. Rust keeps
owned paths. The event callback does not probe or decode photos. The desktop queue starts the
normal background import when the library is ready. Non-UTF-8 paths are reported instead of
silently changing the filename; Unicode, spaces and percent signs are preserved. Missing files
and unavailable volumes follow the importer's error handling.

References: [Apple event manager](https://developer.apple.com/documentation/foundation/nsappleeventmanager),
[AppKit document URLs](https://developer.apple.com/documentation/appkit/nsapplicationdelegate/application(_:open:)),
[system-declared UTIs](https://developer.apple.com/library/archive/documentation/Miscellaneous/Reference/UTIRef/Articles/System-DeclaredUniformTypeIdentifiers.html),
[UTI declarations](https://developer.apple.com/library/archive/documentation/FileManagement/Conceptual/understanding_utis/understand_utis_declare/understand_utis_declare.html),
[handler rank](https://developer.apple.com/documentation/bundleresources/information-property-list/cfbundledocumenttypes/lshandlerrank).

## Native verification

Use an isolated configuration and library, then open a raw through LaunchServices:

```sh
open -n -a /path/to/LightCraft.app \
  --env LIGHTCRAFT_CONFIG_DIR=/path/to/scratch/config \
  /path/to/photo.ARW --args --library /path/to/scratch/library --no-demo --control 18741
open -a /path/to/LightCraft.app /path/to/another-photo.dng
```

Use the control channel to check selection, the displayed photo, duplicate count and import errors.
Test startup with a blocked library as well as a running app, multiple files, an alias, Unicode
names and a missing file. Do not set system handlers. Unregister a temporary test bundle afterwards:

```sh
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -u /path/to/LightCraft.app
```

### Round 1 verification on macOS 26.6.2, Apple Silicon

A local ad-hoc signed bundle was tested with a separate bundle identifier, configuration folder,
and library. `open -a` delivered a cold Sony ARW before the library opened, then a warm panorama
DNG. Both rendered in Detail. The DNG screenshot came from the native Metal window, with no
headless screenshot fallback. LaunchServices listed the bundle for ARW, DNG, JPEG, HEIF, X3F
and KDC; no default handlers were set.

A duplicate ARW opened from Picks kept the catalog at two photos and selected the original.
A request containing a Unicode PNG filename with spaces, `#` and `%`, plus an uppercase JPG,
added both. Opening a Finder alias selected its original without adding another photo. A text file
and an explicit Apple event for a missing ARW produced import errors and left the app responsive.
With a file blocking the requested library folder, a cold DNG and a subsequent warm ARW stayed
queued. Opening a new library drained both requests and selected the ARW.

These checks used the same Open Documents event as Finder and Dock. A physical Finder double-click,
a physical Dock drop, an actual network share and Intel macOS were not tested. The test bundle was
unregistered after the checks. The temporary #990 and #997 build fixes were used only for testing.

### Round 2 verification on macOS 26.6.2, Apple M3 Max

Direct Open Documents events targeted an isolated test process by PID. With Import Photos open,
the native request stayed queued and the catalog remained empty. Switching libraries was refused
with a visible message. Confirming the dialog imported both the manual and native photos into A,
selected the native photo, and rendered Detail. Reopening it kept the count at two. Switching to
B after completion showed an empty library, and reopening A restored both photos.

With a file blocking the startup library folder, a native request waited until another library
was chosen, then imported there. Native Metal screenshots of the blocked switch and Detail were
inspected; neither used the headless fallback. The test process used a separate configuration and
libraries. No bundle was registered, and no defaults were changed.

The regression suite also checks a blocked native worker during manual confirmation, native file
pickers, stale import results after a host replaces its library, all recovery choices, and reopening
the recovery window after choosing Continue Without Saving. Physical Finder/Dock gestures,
network shares, Intel macOS and macOS 11 were not retested in this round.

### Round 3 verification on macOS 26.6.2, Apple M3 Max

A native Open Documents event opened a FIFO to block the real file probe. Cancelling its activity
row allowed an immediate switch to another library. Supplying valid PNG bytes then released the
read; neither the new library nor the original library received a photo. Regression tests also
reject batches prepared before cancellation and wait for a detached worker to exit before checking
both libraries.

With Import Photos open, two native requests produced cancellable activity rows. Quit showed the
queued file count with Quit and Cancel. Cancel kept both requests and the dialog choices. Clicking
the activity rows' cancel buttons discarded the requests while leaving the manual selection intact.
A further single request produced the singular warning, and Quit exited successfully. Cancelling a
queued activity also worked during startup recovery; choosing a usable library afterwards imported
no cancelled file. Native Metal screenshots of the warning and preserved dialog were inspected.
No bundle was registered and no defaults were changed.

The generated plist references AVIF and JPEG XL without imported declarations. The CoreTypes audit
checks their conformance and all relevant tags on macOS 26.6.2. macOS 14 and 15 were not available
for runtime testing. Physical Finder/Dock gestures, network shares and Intel macOS were not retested.
