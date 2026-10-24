package ai.storyteller.lightcraft.mobile;

import android.app.NativeActivity;
import android.content.ClipData;
import android.content.Intent;
import android.database.Cursor;
import android.graphics.Bitmap;
import android.graphics.ImageDecoder;
import android.net.Uri;
import android.os.Build;
import android.provider.DocumentsContract;
import android.provider.MediaStore;
import android.provider.OpenableColumns;
import android.view.ViewGroup;
import android.view.inputmethod.InputMethodManager;
import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.ArrayList;
import java.util.List;

/**
 * NativeActivity plus the photo picker, the share sheet and the soft keyboard's text input
 * ({@link TextInput}). NativeActivity can't host the picker: it never sees
 * onActivityResult. The Rust shell calls {@link #pickPhotos} over JNI and polls
 * {@link #takePicked}; each chosen photo is copied into {@code dir} on a background thread first.
 * HEIC/HEIF (which LightCraft can't decode yet) is converted to JPEG.
 *
 * <p>The document picker ({@link #pickDocuments}, {@link #takeDocuments}) answers the commands
 * that open files or a folder (Import from Folder…, Import Presets…, a GPS track log…): chosen
 * files are copied into a folder the shell names, and a chosen folder's photos (the tree under it)
 * into a folder of that name inside it. Android hands over {@code content://} URIs, not paths.
 *
 * <p>While picked files are being copied in, {@link #copiedFiles}, {@link #filesToCopy} and
 * {@link #copyName} say how
 * far it is (the shell shows it in the activity stack) and {@link #cancelCopy} stops it after the
 * file being copied: what was copied so far is delivered.
 */
public class LightCraftActivity extends NativeActivity {
    private static final int PICK = 0x4c43;
    private static final int DOCS = 0x4c44;
    private static final int TREE = 0x4c45;
    /** How deep a picked folder is copied (a cycle or a pathological tree stops there). */
    private static final int MAX_DEPTH = 24;
    private final List<String[]> documents = new ArrayList<>();
    private String docsDir;
    private String[] treeExtensions = new String[0];
    private final Object lock = new Object();
    private final List<String[]> ready = new ArrayList<>();
    private String dir;
    /** Copies under way (a photo pick and a document pick may overlap), files done and expected. */
    private final java.util.concurrent.atomic.AtomicInteger copying = new java.util.concurrent.atomic.AtomicInteger();
    private final java.util.concurrent.atomic.AtomicInteger copyDone = new java.util.concurrent.atomic.AtomicInteger();
    private final java.util.concurrent.atomic.AtomicInteger copyTotal = new java.util.concurrent.atomic.AtomicInteger();
    private volatile String copyName = "";
    private volatile boolean copyCancel;

    // (anonymous classes, not lambdas: android.jar has no LambdaMetafactory.metafactory for javac)

    /** Show the system photo picker; picked photos are copied into {@code dir}. */
    public void pickPhotos(String dir) {
        this.dir = dir;
        runOnUiThread(new Runnable() {
            @Override
            public void run() {
            Intent intent;
            if (Build.VERSION.SDK_INT >= 33) {
                intent = new Intent(MediaStore.ACTION_PICK_IMAGES);
                intent.putExtra(MediaStore.EXTRA_PICK_IMAGES_MAX, MediaStore.getPickImagesMaxLimit());
            } else {
                intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
                intent.addCategory(Intent.CATEGORY_OPENABLE);
                intent.setType("image/*");
                intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true);
            }
            start(intent, PICK);
            }
        });
    }

    /**
     * Show the system's document picker: {@code kind} 0 = several files, 1 = one file, 2 = a
     * folder. Files are copied into {@code dir}; a folder's files with one of {@code extensions}
     * (comma-separated, lower case) into {@code dir/<its name>}. {@link #takeDocuments} has the
     * answer.
     */
    public void pickDocuments(final int kind, String dir, String extensions) {
        this.docsDir = dir;
        this.treeExtensions = extensions.isEmpty() ? new String[0] : extensions.split(",");
        runOnUiThread(new Runnable() {
            @Override
            public void run() {
                Intent intent;
                if (kind == 2) {
                    intent = new Intent(Intent.ACTION_OPEN_DOCUMENT_TREE);
                    start(intent, TREE);
                } else {
                    intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
                    intent.addCategory(Intent.CATEGORY_OPENABLE);
                    intent.setType("*/*");
                    intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, kind == 0);
                    start(intent, DOCS);
                }
            }
        });
    }

    /**
     * Start a picker for {@code request}. A device without one (some Android Go and TV builds,
     * restricted profiles) answers as a cancelled pick does, so the shell stops waiting.
     */
    private void start(Intent intent, int request) {
        try {
            startActivityForResult(intent, request);
        } catch (android.content.ActivityNotFoundException | SecurityException e) {
            android.util.Log.w("LightCraft", "no picker for " + intent.getAction() + ": " + e);
            onActivityResult(request, RESULT_CANCELED, null);
        }
    }

    /** The paths of one finished document pick (empty when cancelled), or null; oldest first. */
    public String[] takeDocuments() {
        synchronized (lock) {
            return documents.isEmpty() ? null : documents.remove(0);
        }
    }

    /** Open a web link in the browser. */
    public void openUrl(final String url) {
        runOnUiThread(new Runnable() {
            @Override
            public void run() {
                try {
                    startActivity(new Intent(Intent.ACTION_VIEW, Uri.parse(url)));
                } catch (Exception e) {
                    android.util.Log.w("LightCraft", "opening " + url + ": " + e);
                }
            }
        });
    }

    /** Offer exported files (newline-separated paths under the data directory) in the share sheet. */
    public void share(String joined) {
        final ArrayList<Uri> uris = new ArrayList<>();
        String type = null;
        for (String path : joined.split("\n")) {
            if (path.isEmpty()) continue;
            Uri uri = FilesProvider.uriFor(new File(path));
            uris.add(uri);
            String t = getContentResolver().getType(uri);
            type = type == null || type.equals(t) ? t : "image/*";
        }
        if (uris.isEmpty()) return;
        final Intent send = new Intent(uris.size() == 1 ? Intent.ACTION_SEND : Intent.ACTION_SEND_MULTIPLE);
        send.setType(type);
        if (uris.size() == 1) {
            send.putExtra(Intent.EXTRA_STREAM, uris.get(0));
        } else {
            send.putParcelableArrayListExtra(Intent.EXTRA_STREAM, uris);
        }
        // the grant covers every URI in the clip data
        ClipData clip = ClipData.newRawUri(null, uris.get(0));
        for (int i = 1; i < uris.size(); i++) clip.addItem(new ClipData.Item(uris.get(i)));
        send.setClipData(clip);
        send.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
        runOnUiThread(new Runnable() {
            @Override
            public void run() {
                startActivity(Intent.createChooser(send, null));
            }
        });
    }

    private volatile TextInput textInput;

    /** Wakes the Rust event loop (registered by the shell over JNI; see {@code android_jni}). */
    private native void nativeWake();

    /**
     * An egui text field got focus: the soft keyboard types into {@link TextInput} (composing
     * text, suggestions), starting from the field's text and selection (code points), set up for
     * its purpose ({@link TextInput#NORMAL}, {@link TextInput#PASSWORD}, {@link TextInput#TERMINAL},
     * {@link TextInput#NUMBER}).
     */
    public void startTextInput(final String text, final int selStart, final int selEnd, final int purpose) {
        runOnUiThread(new Runnable() {
            @Override
            public void run() {
                if (textInput == null) {
                    textInput = new TextInput(LightCraftActivity.this, new TextInput.Wake() {
                        @Override
                        public void wake() {
                            try {
                                nativeWake();
                            } catch (Throwable ignored) {
                                // not registered yet: the shell polls anyway
                            }
                        }
                    });
                    addContentView(textInput, new ViewGroup.LayoutParams(1, 1));
                }
                TextInput t = textInput;
                t.reset(text, selStart, selEnd, purpose);
                t.setFocusable(true);
                t.setFocusableInTouchMode(true);
                t.requestFocus();
                InputMethodManager imm = (InputMethodManager) getSystemService(INPUT_METHOD_SERVICE);
                imm.restartInput(t);
                imm.showSoftInput(t, 0);
            }
        });
    }

    /**
     * The focused field's text and selection (code points) changed other than by the keyboard's
     * edits: the keyboard's copy follows ({@link TextInput#sync}).
     */
    public void syncTextInput(final String text, final int selStart, final int selEnd) {
        runOnUiThread(new Runnable() {
            @Override
            public void run() {
                TextInput t = textInput;
                if (t != null) t.sync(text, selStart, selEnd);
            }
        });
    }

    /** No egui text field has focus any more: the keyboard goes away. */
    public void stopTextInput() {
        runOnUiThread(new Runnable() {
            @Override
            public void run() {
                TextInput t = textInput;
                if (t == null) return;
                InputMethodManager imm = (InputMethodManager) getSystemService(INPUT_METHOD_SERVICE);
                imm.hideSoftInputFromWindow(t.getWindowToken(), 0);
                // not focusable, or focus would come straight back to it
                t.setFocusable(false);
                t.clearFocus();
                t.clear();
            }
        });
    }

    /** The keyboard's edits since the last call (see {@link TextInput}). */
    public String[] takeTextInput() {
        TextInput t = textInput;
        return t == null ? new String[0] : t.take();
    }

    private volatile int trimLevel;

    @Override
    public void onTrimMemory(int level) {
        super.onTrimMemory(level);
        // running low while in front, or anything after the UI is hidden: shrink caches
        if (level >= TRIM_MEMORY_RUNNING_LOW) trimLevel = Math.max(trimLevel, level);
    }

    @Override
    public void onLowMemory() {
        super.onLowMemory();
        trimLevel = Math.max(trimLevel, TRIM_MEMORY_COMPLETE);
    }

    /** The strongest memory trim level since the last call (0: none), for the Rust side. */
    public int takeTrimLevel() {
        int level = trimLevel;
        trimLevel = 0;
        return level;
    }

    /** How many copies of picked files are under way. */
    public int copiesRunning() {
        return copying.get();
    }

    /** Files copied so far. */
    public int copiedFiles() {
        return copyDone.get();
    }

    /** Files to copy (0: not known yet, a folder being walked). */
    public int filesToCopy() {
        return copyTotal.get();
    }

    /** The file being copied in. */
    public String copyName() {
        return copyName;
    }

    /** Stop copying after the current file (the activity stack's ✕). */
    public void cancelCopy() {
        copyCancel = true;
    }

    /** A copy of {@code files} files (0: not known) starts. */
    private void copyStarted(int files) {
        if (copying.getAndIncrement() == 0) {
            copyCancel = false;
            copyDone.set(0);
            copyTotal.set(0);
        }
        copyTotal.addAndGet(files);
    }

    private void copyEnded() {
        copying.decrementAndGet();
    }

    /** The paths of one finished selection (empty when cancelled), or null; oldest first. */
    public String[] takePicked() {
        synchronized (lock) {
            return ready.isEmpty() ? null : ready.remove(0);
        }
    }

    @Override
    protected void onActivityResult(int request, int result, Intent data) {
        super.onActivityResult(request, result, data);
        if (request == DOCS || request == TREE) {
            documentsResult(request, result, data);
            return;
        }
        if (request != PICK) return;
        if (result != RESULT_OK || data == null) {
            finished(new ArrayList<>()); // cancelled: the shell stops waiting
            return;
        }
        List<Uri> uris = new ArrayList<>();
        ClipData clip = data.getClipData();
        if (clip != null) {
            for (int i = 0; i < clip.getItemCount(); i++) uris.add(clip.getItemAt(i).getUri());
        } else if (data.getData() != null) {
            uris.add(data.getData());
        }
        final String target = dir;
        final List<Uri> picked = uris;
        new Thread(new Runnable() {
            @Override
            public void run() {
            List<String> paths = new ArrayList<>();
            copyStarted(picked.size());
            try {
                for (Uri uri : picked) {
                    if (copyCancel) {
                        android.util.Log.i("LightCraft", "copy stopped after " + paths.size() + " of " + picked.size() + " photos");
                        break;
                    }
                    try {
                        paths.add(copy(uri, target));
                    } catch (Exception e) {
                        android.util.Log.w("LightCraft", "copying " + uri + ": " + e);
                    }
                    copyDone.incrementAndGet();
                }
            } finally {
                copyEnded();
            }
            finished(paths);
            }
        }).start();
    }

    private void documentsResult(final int request, int result, Intent data) {
        final List<Uri> uris = new ArrayList<>();
        if (result == RESULT_OK && data != null) {
            ClipData clip = data.getClipData();
            if (clip != null) {
                for (int i = 0; i < clip.getItemCount(); i++) uris.add(clip.getItemAt(i).getUri());
            } else if (data.getData() != null) {
                uris.add(data.getData());
            }
        }
        final String target = docsDir;
        final String[] exts = treeExtensions;
        new Thread(new Runnable() {
            @Override
            public void run() {
                List<String> paths = new ArrayList<>();
                // a folder's file count isn't known until it has been walked
                copyStarted(request == TREE ? 0 : uris.size());
                for (Uri uri : uris) {
                    if (copyCancel) break;
                    try {
                        if (request == TREE) {
                            String root = DocumentsContract.getTreeDocumentId(uri);
                            String name = treeName(uri, root);
                            File out = free(new File(target), name);
                            copyTree(uri, root, out, exts, 0);
                            out.mkdirs();
                            paths.add(out.getAbsolutePath());
                        } else {
                            paths.add(copyDocument(uri, new File(target)));
                            copyDone.incrementAndGet();
                        }
                    } catch (Exception e) {
                        android.util.Log.w("LightCraft", "copying " + uri + ": " + e);
                    }
                }
                copyEnded();
                synchronized (lock) {
                    documents.add(paths.toArray(new String[0]));
                }
                try {
                    nativeWake();
                } catch (Throwable ignored) {
                    // not registered yet: the shell polls anyway
                }
            }
        }).start();
    }

    /** A picked folder's own name (its document's display name), for its copy. */
    private String treeName(Uri tree, String root) {
        Uri doc = DocumentsContract.buildDocumentUriUsingTree(tree, root);
        try (Cursor c = getContentResolver().query(doc, new String[] {DocumentsContract.Document.COLUMN_DISPLAY_NAME}, null, null, null)) {
            if (c != null && c.moveToFirst() && c.getString(0) != null) return safeName(c.getString(0));
        } catch (Exception ignored) {
        }
        return "Folder";
    }

    /** Copy the files under {@code parent} (a document in {@code tree}) with one of {@code exts} into {@code out}. */
    private void copyTree(Uri tree, String parent, File out, String[] exts, int depth) throws Exception {
        if (depth > MAX_DEPTH) return;
        Uri children = DocumentsContract.buildChildDocumentsUriUsingTree(tree, parent);
        String[] cols = {DocumentsContract.Document.COLUMN_DOCUMENT_ID, DocumentsContract.Document.COLUMN_DISPLAY_NAME, DocumentsContract.Document.COLUMN_MIME_TYPE};
        try (Cursor c = getContentResolver().query(children, cols, null, null, null)) {
            if (c == null) return;
            while (c.moveToNext()) {
                String id = c.getString(0), name = c.getString(1), mime = c.getString(2);
                if (id == null || name == null) continue;
                name = safeName(name);
                if (DocumentsContract.Document.MIME_TYPE_DIR.equals(mime)) {
                    copyTree(tree, id, new File(out, name), exts, depth + 1);
                } else if (wanted(name, exts)) {
                    if (copyCancel) return;
                    out.mkdirs();
                    File file = new File(out, name);
                    if (file.exists()) continue;
                    copyName = name;
                    write(DocumentsContract.buildDocumentUriUsingTree(tree, id), file);
                    copyDone.incrementAndGet();
                }
            }
        }
    }

    private static boolean wanted(String name, String[] exts) {
        if (exts.length == 0) return true;
        int dot = name.lastIndexOf('.');
        if (dot < 0) return false;
        String ext = name.substring(dot + 1).toLowerCase(java.util.Locale.ROOT);
        for (String e : exts) if (e.equals(ext)) return true;
        return false;
    }

    /** A file name without path separators (names come from other apps). */
    private static String safeName(String name) {
        String s = name.replace('/', '_').replace('\\', '_');
        return s.isEmpty() || s.equals(".") || s.equals("..") ? "_" : s;
    }

    /** Copy one picked document into {@code out} under its own name; its path. */
    private String copyDocument(Uri uri, File out) throws Exception {
        out.mkdirs();
        String name = safeName(displayName(uri));
        copyName = name;
        File file = free(out, name);
        write(uri, file);
        return file.getAbsolutePath();
    }

    /** Copy {@code uri} into {@code file}; a copy cut short (an error) leaves nothing behind. */
    private void write(Uri uri, File file) throws Exception {
        try (InputStream in = getContentResolver().openInputStream(uri); OutputStream os = new FileOutputStream(file)) {
            if (in == null) throw new java.io.IOException("can't be opened");
            byte[] buf = new byte[1 << 16];
            for (int n; (n = in.read(buf)) > 0; ) os.write(buf, 0, n);
        } catch (Exception e) {
            file.delete();
            throw e;
        }
    }

    private void finished(List<String> paths) {
        synchronized (lock) {
            ready.add(paths.toArray(new String[0]));
        }
    }

    private String copy(Uri uri, String target) throws Exception {
        File out = new File(target);
        out.mkdirs();
        String name = safeName(displayName(uri));
        copyName = name;
        String type = getContentResolver().getType(uri);
        boolean heif = type != null && (type.equals("image/heic") || type.equals("image/heif"));
        if (heif) name = name.replaceAll("\\.(?i)(heic|heif)$", "") + ".jpg";
        File file = free(out, name);
        if (heif) {
            Bitmap bitmap = ImageDecoder.decodeBitmap(ImageDecoder.createSource(getContentResolver(), uri));
            try (OutputStream os = new FileOutputStream(file)) {
                bitmap.compress(Bitmap.CompressFormat.JPEG, 95, os);
            } catch (Exception e) {
                file.delete();
                throw e;
            }
        } else {
            write(uri, file);
        }
        return file.getAbsolutePath();
    }

    private String displayName(Uri uri) {
        try (Cursor c = getContentResolver().query(uri, new String[] {OpenableColumns.DISPLAY_NAME}, null, null, null)) {
            if (c != null && c.moveToFirst() && c.getString(0) != null) return c.getString(0);
        } catch (Exception ignored) {
        }
        return "photo-" + System.currentTimeMillis() + ".jpg";
    }

    /** {@code name} in {@code dir}, numbered when taken (picking a photo again keeps the first copy). */
    private static File free(File dir, String name) {
        File f = new File(dir, name);
        int dot = name.lastIndexOf('.');
        String stem = dot > 0 ? name.substring(0, dot) : name, ext = dot > 0 ? name.substring(dot) : "";
        for (int n = 1; f.exists(); n++) f = new File(dir, stem + "-" + n + ext);
        return f;
    }
}
