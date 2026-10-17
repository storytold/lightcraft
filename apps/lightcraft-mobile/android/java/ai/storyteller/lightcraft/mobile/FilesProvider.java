package ai.storyteller.lightcraft.mobile;

import android.content.ContentProvider;
import android.content.ContentValues;
import android.database.Cursor;
import android.database.MatrixCursor;
import android.net.Uri;
import android.os.ParcelFileDescriptor;
import android.provider.OpenableColumns;
import android.webkit.MimeTypeMap;
import java.io.File;
import java.io.FileNotFoundException;
import java.io.IOException;

/**
 * Read-only access to files under the app's data directory, for the share sheet: other apps get
 * {@code content://ai.storyteller.lightcraft.mobile.files/<path>} with a temporary read grant.
 * (androidx's FileProvider without androidx: the APK is built without Gradle.)
 */
public class FilesProvider extends ContentProvider {
    static final String AUTHORITY = "ai.storyteller.lightcraft.mobile.files";

    static Uri uriFor(File file) {
        return new Uri.Builder().scheme("content").authority(AUTHORITY).path(file.getAbsolutePath()).build();
    }

    /** The file a URI names, if it's inside the app's data directory. */
    private File fileFor(Uri uri) throws FileNotFoundException {
        try {
            File file = new File(uri.getPath()).getCanonicalFile();
            String root = getContext().getFilesDir().getCanonicalPath() + File.separator;
            if (file.getPath().startsWith(root) && file.isFile()) return file;
        } catch (IOException ignored) {
        }
        throw new FileNotFoundException(uri.toString());
    }

    @Override
    public boolean onCreate() {
        return true;
    }

    @Override
    public ParcelFileDescriptor openFile(Uri uri, String mode) throws FileNotFoundException {
        if (!"r".equals(mode)) throw new SecurityException("read-only");
        return ParcelFileDescriptor.open(fileFor(uri), ParcelFileDescriptor.MODE_READ_ONLY);
    }

    @Override
    public Cursor query(Uri uri, String[] projection, String selection, String[] args, String sort) {
        File file;
        try {
            file = fileFor(uri);
        } catch (FileNotFoundException e) {
            return null;
        }
        String[] cols = projection != null ? projection : new String[] {OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE};
        Object[] row = new Object[cols.length];
        for (int i = 0; i < cols.length; i++) {
            if (OpenableColumns.DISPLAY_NAME.equals(cols[i])) row[i] = file.getName();
            else if (OpenableColumns.SIZE.equals(cols[i])) row[i] = file.length();
        }
        MatrixCursor c = new MatrixCursor(cols, 1);
        c.addRow(row);
        return c;
    }

    @Override
    public String getType(Uri uri) {
        String name = uri.getLastPathSegment();
        int dot = name == null ? -1 : name.lastIndexOf('.');
        if (dot < 0) return "application/octet-stream";
        String ext = name.substring(dot + 1).toLowerCase();
        if (ext.equals("dng")) return "image/x-adobe-dng";
        String type = MimeTypeMap.getSingleton().getMimeTypeFromExtension(ext);
        return type != null ? type : "application/octet-stream";
    }

    @Override
    public Uri insert(Uri uri, ContentValues values) {
        throw new UnsupportedOperationException("read-only");
    }

    @Override
    public int delete(Uri uri, String selection, String[] args) {
        throw new UnsupportedOperationException("read-only");
    }

    @Override
    public int update(Uri uri, ContentValues values, String selection, String[] args) {
        throw new UnsupportedOperationException("read-only");
    }
}
