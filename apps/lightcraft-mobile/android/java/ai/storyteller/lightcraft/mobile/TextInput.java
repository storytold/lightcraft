package ai.storyteller.lightcraft.mobile;

import android.content.Context;
import android.text.Editable;
import android.text.InputType;
import android.text.Selection;
import android.view.KeyEvent;
import android.view.View;
import android.view.inputmethod.BaseInputConnection;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputConnection;
import android.view.inputmethod.InputMethodManager;
import java.util.ArrayList;
import java.util.List;

/**
 * The soft keyboard's text editor for egui text fields. NativeActivity's own view has no input
 * connection, so the keyboard falls back to key events: no composing (CJK, Korean, Indic
 * scripts), autocorrect or swipe typing. This invisible view takes the keyboard while an egui text
 * field has focus; its input connection keeps a shadow copy of what was typed (the keyboard reads
 * it for suggestions) and queues every edit for the Rust shell, which polls {@link #take} and turns
 * them into egui IME events:
 *
 * <ul>
 *   <li>{@code P<text>}: the composing (pre-edit) text is now {@code text};
 *   <li>{@code C<text>}: commit {@code text} (in place of the composing text);
 *   <li>{@code D<before>,<after>}: delete that many characters (code points) around the cursor.
 * </ul>
 *
 * Key events the keyboard sends (Delete with nothing left to delete, Enter) go to the native input
 * queue like hardware keys.
 *
 * <p>The copy starts as the field's text and cursor ({@link #reset}) and follows changes made outside
 * the keyboard (taps moving the cursor, keys, the field cleared: {@link #sync}), so what the keyboard
 * reads back is what the field shows.
 */
final class TextInput extends View {
    /** Called (on the UI thread) whenever edits were queued: the Rust side wakes its event loop. */
    interface Wake {
        void wake();
    }

    private final Object lock = new Object();
    private final List<String> queue = new ArrayList<>();
    private final Wake wake;
    /** Edits queued so far, and how many of them the shell had taken at its last {@link #take}. */
    private long queued, taken;
    /** The field's text and selection (UTF-16) the next input connection starts from. */
    private String text = "";
    private int selStart, selEnd;
    /**
     * What the field is for: {@link #NORMAL}, {@link #PASSWORD}, {@link #TERMINAL} (egui's IME
     * purpose) or {@link #NUMBER} (a number being typed into an egui drag value).
     */
    private int purpose;

    static final int NORMAL = 0, PASSWORD = 1, TERMINAL = 2, NUMBER = 3;
    private Connection connection;

    TextInput(Context context, Wake wake) {
        super(context);
        this.wake = wake;
        setFocusable(true);
        setFocusableInTouchMode(true);
    }

    /** The edits queued since the last call, oldest first. */
    String[] take() {
        synchronized (lock) {
            String[] out = queue.toArray(new String[0]);
            queue.clear();
            taken = queued;
            return out;
        }
    }

    void clear() {
        synchronized (lock) {
            queue.clear();
            taken = queued;
        }
    }

    private void push(String edit) {
        synchronized (lock) {
            queue.add(edit);
            queued++;
        }
        wake.wake();
    }

    /** UTF-16 index of code point {@code cp} in {@code s} (clamped). */
    private static int utf16(String s, int cp) {
        return s.offsetByCodePoints(0, Math.max(0, Math.min(cp, s.codePointCount(0, s.length()))));
    }

    /**
     * A field got focus: the next input connection starts from its text and selection ({@code a},
     * {@code b}: code points, as egui counts) and is set up for its {@code purpose}. UI thread,
     * before restarting input.
     */
    void reset(String text, int a, int b, int purpose) {
        clear();
        this.purpose = purpose;
        this.text = text;
        selStart = utf16(text, Math.min(a, b));
        selEnd = utf16(text, Math.max(a, b));
    }

    /**
     * The field now holds {@code text} with selection {@code a}–{@code b} (code points). When that
     * isn't what the keyboard's edits left (the cursor moved by a tap, a key, the field cleared),
     * the copy takes it and the keyboard is told; a composition in progress ends there (the field
     * keeps its text). Ignored while the shell hasn't seen every edit yet: it reports a state
     * from before them. UI thread.
     */
    void sync(String text, int a, int b) {
        synchronized (lock) {
            if (queued != taken || !queue.isEmpty()) return;
        }
        Connection c = connection;
        if (c == null) {
            reset(text, a, b, purpose);
            return;
        }
        Editable e = c.getEditable();
        int sa = utf16(text, Math.min(a, b)), sb = utf16(text, Math.max(a, b));
        boolean same = e.toString().equals(text);
        int cs = BaseInputConnection.getComposingSpanStart(e), ce = BaseInputConnection.getComposingSpanEnd(e);
        if (same) {
            int ea = Selection.getSelectionStart(e), eb = Selection.getSelectionEnd(e);
            // the field shows a composition as its selection
            if ((ea == sa && eb == sb) || (cs >= 0 && Math.min(cs, ce) == sa && Math.max(cs, ce) == sb)) return;
        }
        BaseInputConnection.removeComposingSpans(e);
        if (!same) e.replace(0, e.length(), text);
        Selection.setSelection(e, sa, sb);
        c.report(true);
    }

    @Override
    public boolean onCheckIsTextEditor() {
        return true;
    }

    @Override
    public InputConnection onCreateInputConnection(EditorInfo info) {
        // Enter is a key (egui decides: a new line, or done)
        info.imeOptions = EditorInfo.IME_FLAG_NO_FULLSCREEN | EditorInfo.IME_FLAG_NO_EXTRACT_UI | EditorInfo.IME_FLAG_NO_ENTER_ACTION
                | EditorInfo.IME_ACTION_NONE;
        switch (purpose) {
            case PASSWORD:
                // no suggestions, nothing learned or remembered (the copy holds egui's masked text)
                info.inputType = InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_PASSWORD;
                info.imeOptions |= EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING;
                break;
            case TERMINAL:
                // typed as is: no suggestions or corrections
                info.inputType = InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS;
                break;
            case NUMBER:
                // the number pad, with a minus sign and a decimal point (drag values can be both)
                info.inputType = InputType.TYPE_CLASS_NUMBER | InputType.TYPE_NUMBER_FLAG_SIGNED | InputType.TYPE_NUMBER_FLAG_DECIMAL;
                break;
            default:
                // plain text with suggestions
                info.inputType = InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_AUTO_CORRECT;
        }
        Connection c = new Connection();
        Editable e = c.getEditable();
        e.replace(0, e.length(), text);
        Selection.setSelection(e, selStart, selEnd);
        info.initialSelStart = selStart;
        info.initialSelEnd = selEnd;
        connection = c;
        return c;
    }

    /** Code points in {@code s} from {@code a} to {@code b} (UTF-16 indices, clamped). */
    private static int codePoints(CharSequence s, int a, int b) {
        a = Math.max(0, Math.min(a, s.length()));
        b = Math.max(a, Math.min(b, s.length()));
        return Character.codePointCount(s, a, b);
    }

    private final class Connection extends BaseInputConnection {
        Connection() {
            super(TextInput.this, true);
        }

        /**
         * Tell the keyboard where the cursor and the composing text are now: it relies on these
         * reports (a text view's) to compose words and to replace them with suggestions.
         */
        private boolean report(boolean ok) {
            Editable e = getEditable();
            InputMethodManager imm = (InputMethodManager) getContext().getSystemService(Context.INPUT_METHOD_SERVICE);
            if (imm != null) {
                imm.updateSelection(TextInput.this, Selection.getSelectionStart(e), Selection.getSelectionEnd(e), getComposingSpanStart(e),
                        getComposingSpanEnd(e));
            }
            return ok;
        }

        private String composing() {
            Editable e = getEditable();
            int a = getComposingSpanStart(e), b = getComposingSpanEnd(e);
            return a < 0 || b < 0 ? "" : e.subSequence(Math.min(a, b), Math.max(a, b)).toString();
        }

        @Override
        public boolean setComposingText(CharSequence text, int newCursorPosition) {
            push("P" + text);
            return report(super.setComposingText(text, newCursorPosition));
        }

        @Override
        public boolean commitText(CharSequence text, int newCursorPosition) {
            push("C" + text);
            return report(super.commitText(text, newCursorPosition));
        }

        @Override
        public boolean finishComposingText() {
            String c = composing();
            if (!c.isEmpty()) push("C" + c);
            return report(super.finishComposingText());
        }

        @Override
        public boolean setComposingRegion(int start, int end) {
            // the keyboard re-opens a word already typed (e.g. backspacing into it): it becomes the
            // composing text again
            Editable e = getEditable();
            int a = Math.max(0, Math.min(Math.min(start, end), e.length())), b = Math.max(a, Math.min(Math.max(start, end), e.length()));
            int cursor = Selection.getSelectionEnd(e);
            if (b > a && cursor >= 0) {
                String word = e.subSequence(a, b).toString();
                int before = cursor > a ? codePoints(e, a, cursor) : 0;
                int after = b > cursor ? codePoints(e, cursor, b) : 0;
                push("D" + before + "," + after);
                push("P" + word);
            }
            return report(super.setComposingRegion(start, end));
        }

        @Override
        public boolean deleteSurroundingText(int before, int after) {
            // UTF-16 units → code points, over the shadow copy (text from before the field got
            // focus isn't in it: one unit each)
            Editable e = getEditable();
            int cursor = Math.max(0, Selection.getSelectionStart(e));
            int end = Math.max(cursor, Selection.getSelectionEnd(e));
            int inBefore = Math.min(before, cursor), inAfter = Math.min(after, e.length() - end);
            int b = codePoints(e, cursor - inBefore, cursor) + (before - inBefore);
            int a = codePoints(e, end, end + inAfter) + (after - inAfter);
            if (b > 0 || a > 0) push("D" + b + "," + a);
            return report(super.deleteSurroundingText(before, after));
        }

        @Override
        public boolean deleteSurroundingTextInCodePoints(int before, int after) {
            if (before > 0 || after > 0) push("D" + before + "," + after);
            return report(super.deleteSurroundingTextInCodePoints(before, after));
        }

        @Override
        public boolean setSelection(int start, int end) {
            // (egui keeps its own cursor: the keyboard moving it isn't passed on)
            return report(super.setSelection(start, end));
        }

        @Override
        public boolean performEditorAction(int action) {
            // (no action is offered, but some keyboards send one for Enter anyway)
            long t = android.os.SystemClock.uptimeMillis();
            sendKeyEvent(new KeyEvent(t, t, KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_ENTER, 0));
            sendKeyEvent(new KeyEvent(t, t, KeyEvent.ACTION_UP, KeyEvent.KEYCODE_ENTER, 0));
            return true;
        }
    }
}
