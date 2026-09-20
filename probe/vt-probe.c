/* Headless libghostty-vt feasibility probe, not font/rendering or display proof.
 * API pin: c5a21edfcbc2d5b46540ad91b7980aca31f5f1f3. C11 + POSIX libc.
 * No PTY, UI, transport, VTE, or test framework; checks survive NDEBUG.
 */
#define _POSIX_C_SOURCE 200809L
#include <errno.h>
#include <inttypes.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <time.h>
#include <ghostty/vt/terminal.h>
#include <ghostty/vt/render.h>
#include <ghostty/vt/key/encoder.h>
#include <ghostty/vt/paste.h>
#include <ghostty/vt/sgr.h>

enum { COLS = 32, ROWS = 6 };
static GhosttyTerminal terminal;
static GhosttyRenderState render;
static GhosttyRenderStateRowIterator rows;
static GhosttyRenderStateRowCells cells;
static GhosttyKeyEncoder encoder;
static GhosttyKeyEvent event;
static const char *case_name = "setup";
static unsigned checks;

static void cleanup(void)
{
    ghostty_key_event_free(event);
    ghostty_key_encoder_free(encoder);
    ghostty_render_state_row_cells_free(cells);
    ghostty_render_state_row_iterator_free(rows);
    ghostty_render_state_free(render);
    ghostty_terminal_free(terminal);
}

static void equal(const char *what, int64_t actual, int64_t expected)
{
    ++checks;
    if (actual != expected) {
        fprintf(stderr, "FAIL case=%s %s expected=%" PRId64 " actual=%" PRId64 "\n",
                case_name, what, expected, actual);
        exit(EXIT_FAILURE);
    }
}

#define OK(call) equal(#call, (call), GHOSTTY_SUCCESS)
#define FEED(literal) ghostty_terminal_vt_write(terminal, \
    (const uint8_t *)(literal), sizeof(literal) - 1)

static void hex(FILE *out, const void *data, size_t len)
{
    const unsigned char *bytes = data;
    for (size_t i = 0; i < len; ++i)
        fprintf(out, "%s%02x", i ? " " : "", bytes[i]);
}

static void bytes_equal(const char *what, const void *actual, size_t actual_len,
                        const void *expected, size_t expected_len)
{
    ++checks;
    if (actual_len != expected_len || memcmp(actual, expected, expected_len)) {
        fprintf(stderr, "FAIL case=%s %s expected[%zu]=", case_name, what, expected_len);
        hex(stderr, expected, expected_len);
        fprintf(stderr, " actual[%zu]=", actual_len);
        hex(stderr, actual, actual_len);
        fputc('\n', stderr);
        exit(EXIT_FAILURE);
    }
}

static GhosttyGridRef ref_at(uint16_t x, uint16_t y)
{
    GhosttyGridRef ref = GHOSTTY_INIT_SIZED(GhosttyGridRef);
    GhosttyPoint point = { .tag = GHOSTTY_POINT_TAG_ACTIVE,
        .value.coordinate = { .x = x, .y = y } };
    OK(ghostty_terminal_grid_ref(terminal, point, &ref));
    return ref;
}

static GhosttyCell cell_at(uint16_t x, uint16_t y)
{
    GhosttyGridRef ref = ref_at(x, y);
    GhosttyCell cell = 0;
    OK(ghostty_grid_ref_cell(&ref, &cell));
    return cell;
}

static void codepoint(uint16_t x, uint16_t y, uint32_t expected)
{
    uint32_t actual = 0;
    char label[64];
    OK(ghostty_cell_get(cell_at(x, y), GHOSTTY_CELL_DATA_CODEPOINT, &actual));
    snprintf(label, sizeof(label), "cell[%u,%u] codepoint", x, y);
    equal(label, actual, expected);
}

static void width(uint16_t x, GhosttyCellWide expected)
{
    GhosttyCellWide actual;
    OK(ghostty_cell_get(cell_at(x, 0), GHOSTTY_CELL_DATA_WIDE, &actual));
    equal("cell width tag", actual, expected);
}

static void cursor(uint16_t x, uint16_t y)
{
    uint16_t actual_x = 0, actual_y = 0;
    OK(ghostty_terminal_get(terminal, GHOSTTY_TERMINAL_DATA_CURSOR_X, &actual_x));
    OK(ghostty_terminal_get(terminal, GHOSTTY_TERMINAL_DATA_CURSOR_Y, &actual_y));
    equal("cursor x", actual_x, x);
    equal("cursor y", actual_y, y);
}

static void screen(GhosttyTerminalScreen expected)
{
    GhosttyTerminalScreen actual;
    OK(ghostty_terminal_get(terminal, GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN, &actual));
    equal("active screen", actual, expected);
}

static bool mode(GhosttyMode which, bool expected)
{
    bool actual = false;
    OK(ghostty_terminal_mode_get(terminal, which, &actual));
    equal("terminal mode", actual, expected);
    return actual;
}

static void select_render_cell(uint16_t x, uint16_t y)
{
    OK(ghostty_render_state_get(render, GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR, &rows));
    for (uint16_t i = 0; i <= y; ++i)
        equal("render row exists", ghostty_render_state_row_iterator_next(rows), true);
    OK(ghostty_render_state_row_get(rows, GHOSTTY_RENDER_STATE_ROW_DATA_CELLS, &cells));
    OK(ghostty_render_state_row_cells_select(cells, x));
}

static void render_text(uint16_t x, uint16_t y, const char *expected)
{
    uint8_t data[64];
    GhosttyBuffer buffer = { .ptr = data, .cap = sizeof(data), .len = 0 };
    select_render_cell(x, y);
    OK(ghostty_render_state_row_cells_get(cells,
        GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_UTF8, &buffer));
    equal("render UTF8 fits buffer", buffer.len <= sizeof(data), true);
    bytes_equal("render grapheme UTF8", data, buffer.len, expected, strlen(expected));
}

static void split_and_unicode(void)
{
    case_name = "split-escape-utf8";
    FEED("\x1b[");
    cursor(0, 0);
    codepoint(0, 0, 0);
    FEED("2;3");
    cursor(0, 0);
    FEED("HX");
    codepoint(2, 1, 'X');
    cursor(3, 1);
    ghostty_terminal_reset(terminal);
    FEED("e\xcc");
    cursor(1, 0);
    codepoint(1, 0, 0);
    FEED("\x81\xe7");
    cursor(1, 0);
    FEED("\x95");
    cursor(1, 0);
    FEED("\x8c!");
    codepoint(0, 0, 'e');
    codepoint(1, 0, 0x754c);
    codepoint(3, 0, '!');
    codepoint(4, 0, 0);
    width(0, GHOSTTY_CELL_WIDE_NARROW);
    width(1, GHOSTTY_CELL_WIDE_WIDE);
    width(2, GHOSTTY_CELL_WIDE_SPACER_TAIL);
    cursor(4, 0);
    GhosttyGridRef ref = ref_at(0, 0);
    uint32_t graphemes[4];
    size_t len = 0;
    OK(ghostty_grid_ref_graphemes(&ref, graphemes, 4, &len));
    equal("combining grapheme count", len, 2);
    equal("combining base", graphemes[0], 'e');
    equal("combining accent", graphemes[1], 0x301);
    OK(ghostty_render_state_update(render, terminal));
    render_text(0, 0, "e\xcc\x81");
    render_text(1, 0, "\xe7\x95\x8c");
    render_text(3, 0, "!");
    bool positioned = false;
    uint16_t x = 0, y = 0;
    OK(ghostty_render_state_get(render,
        GHOSTTY_RENDER_STATE_DATA_CURSOR_VIEWPORT_HAS_VALUE, &positioned));
    equal("render cursor in viewport", positioned, true);
    OK(ghostty_render_state_get(render, GHOSTTY_RENDER_STATE_DATA_CURSOR_VIEWPORT_X, &x));
    OK(ghostty_render_state_get(render, GHOSTTY_RENDER_STATE_DATA_CURSOR_VIEWPORT_Y, &y));
    equal("render cursor x", x, 4);
    equal("render cursor y", y, 0);
    puts("CASE split-escape-utf8 OK: incomplete CSI/UTF8 held; e+U+0301; U+754C wide+tail; cursor=(4,0); render graphemes exact");
}

static void hyperlinks(void)
{
    case_name = "osc8";
    ghostty_terminal_reset(terminal);
    FEED("\x1b]8;id=probe;https://example.org/probe\x1b");
    cursor(0, 0);
    FEED("\\link\x1b]8;;\x1b");
    cursor(4, 0);
    FEED("\\!");
    cursor(5, 0);
    for (uint16_t y = 0; y < ROWS; ++y)
        for (uint16_t x = 0; x < COLS; ++x)
            codepoint(x, y, y == 0 && x < 5 ? (uint32_t)"link!"[x] : 0);
    for (uint16_t x = 0; x < 5; ++x) {
        GhosttyGridRef ref = ref_at(x, 0);
        uint8_t uri[128];
        size_t len = 0;
        bool linked = false;
        OK(ghostty_cell_get(cell_at(x, 0), GHOSTTY_CELL_DATA_HAS_HYPERLINK, &linked));
        equal("cell hyperlink present", linked, x < 4);
        OK(ghostty_grid_ref_hyperlink_uri(&ref, uri, sizeof(uri), &len));
        equal("URI fits buffer", len <= sizeof(uri), true);
        const char *expected = x < 4 ? "https://example.org/probe" : "";
        bytes_equal("OSC8 URI", uri, len, expected, strlen(expected));
    }
    OK(ghostty_render_state_update(render, terminal));
    for (uint16_t x = 0; x < 5; ++x) {
        char text[2] = { "link!"[x], 0 };
        render_text(x, 0, text);
    }
    puts("CASE osc8 OK: split ST parsed; only link! visible; link cells carry exact URI; ! unlinked");
    puts("LIMIT osc8: public cell API exposes URI/presence, not OSC8 id parameter; no id round-trip asserted");
}

static void alternate_screen(void)
{
    case_name = "alternate-screen";
    ghostty_terminal_reset(terminal);
    FEED("MAIN\x1b[3;7H");
    cursor(6, 2);
    screen(GHOSTTY_TERMINAL_SCREEN_PRIMARY);
    FEED("\x1b[?1049h\x1b[H");
    screen(GHOSTTY_TERMINAL_SCREEN_ALTERNATE);
    codepoint(0, 0, 0);
    FEED("ALT");
    codepoint(0, 0, 'A');
    cursor(3, 0);
    OK(ghostty_render_state_update(render, terminal));
    render_text(0, 0, "A");
    FEED("\x1b[?1049l");
    screen(GHOSTTY_TERMINAL_SCREEN_PRIMARY);
    cursor(6, 2);
    for (uint16_t y = 0; y < ROWS; ++y)
        for (uint16_t x = 0; x < COLS; ++x)
            codepoint(x, y, y == 0 && x < 4 ? (uint32_t)"MAIN"[x] : 0);
    OK(ghostty_render_state_update(render, terminal));
    render_text(0, 0, "M");
    puts("CASE alternate-screen OK: isolated ALT; primary MAIN and cursor=(6,2) restored through DEC1049");
}

static void check_style(const GhosttyStyle *style)
{
    equal("bold", style->bold, true);
    equal("italic", style->italic, true);
    equal("underline", style->underline, GHOSTTY_SGR_UNDERLINE_SINGLE);
    equal("foreground tag", style->fg_color.tag, GHOSTTY_STYLE_COLOR_RGB);
    equal("foreground red", style->fg_color.value.rgb.r, 12);
    equal("foreground green", style->fg_color.value.rgb.g, 34);
    equal("foreground blue", style->fg_color.value.rgb.b, 56);
    equal("background tag", style->bg_color.tag, GHOSTTY_STYLE_COLOR_PALETTE);
    equal("background palette", style->bg_color.value.palette, 4);
}

static void styles(void)
{
    case_name = "styles";
    ghostty_terminal_reset(terminal);
    FEED("\x1b[1;3;4;38;2;12;34;56;48;5;4mS\x1b[0mN");
    codepoint(0, 0, 'S');
    codepoint(1, 0, 'N');
    GhosttyGridRef ref = ref_at(0, 0);
    GhosttyStyle style = GHOSTTY_INIT_SIZED(GhosttyStyle);
    OK(ghostty_grid_ref_style(&ref, &style));
    check_style(&style);
    ref = ref_at(1, 0);
    OK(ghostty_grid_ref_style(&ref, &style));
    equal("SGR0 default style", ghostty_style_is_default(&style), true);
    OK(ghostty_render_state_update(render, terminal));
    select_render_cell(0, 0);
    OK(ghostty_render_state_row_cells_get(cells,
        GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE, &style));
    check_style(&style);
    GhosttyColorRgb fg;
    OK(ghostty_render_state_row_cells_get(cells,
        GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_FG_COLOR, &fg));
    equal("resolved foreground red", fg.r, 12);
    equal("resolved foreground green", fg.g, 34);
    equal("resolved foreground blue", fg.b, 56);
    puts("CASE styles OK: bold+italic+underline, RGB(12,34,56), palette background=4; SGR0 reset; render style agrees");
}

static void dirty_state(GhosttyRenderStateDirty expected)
{
    GhosttyRenderStateDirty actual;
    OK(ghostty_render_state_get(render, GHOSTTY_RENDER_STATE_DATA_DIRTY, &actual));
    equal("render dirty state", actual, expected);
}

static void dirty_rows(bool first_dirty, bool acknowledge)
{
    unsigned count = 0;
    OK(ghostty_render_state_get(render, GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR, &rows));
    while (ghostty_render_state_row_iterator_next(rows)) {
        bool actual = false;
        OK(ghostty_render_state_row_get(rows, GHOSTTY_RENDER_STATE_ROW_DATA_DIRTY, &actual));
        if (!acknowledge)
            equal("row dirty flag", actual, count == 0 && first_dirty);
        if (acknowledge) {
            const bool clean = false;
            OK(ghostty_render_state_row_set(rows, GHOSTTY_RENDER_STATE_ROW_OPTION_DIRTY, &clean));
        }
        ++count;
    }
    equal("render row count", count, ROWS);
}

static void dirty_updates(void)
{
    case_name = "dirty-updates";
    ghostty_terminal_reset(terminal);
    FEED("A");
    OK(ghostty_render_state_update(render, terminal));
    dirty_state(GHOSTTY_RENDER_STATE_DIRTY_FULL);
    uint16_t cols = 0, height = 0;
    OK(ghostty_render_state_get(render, GHOSTTY_RENDER_STATE_DATA_COLS, &cols));
    OK(ghostty_render_state_get(render, GHOSTTY_RENDER_STATE_DATA_ROWS, &height));
    equal("render columns", cols, COLS);
    equal("render height", height, ROWS);
    dirty_rows(false, true);
    const GhosttyRenderStateDirty clean = GHOSTTY_RENDER_STATE_DIRTY_FALSE;
    OK(ghostty_render_state_set(render, GHOSTTY_RENDER_STATE_OPTION_DIRTY, &clean));
    OK(ghostty_render_state_update(render, terminal));
    dirty_state(GHOSTTY_RENDER_STATE_DIRTY_FALSE);
    dirty_rows(false, false);
    FEED("\bB");
    cursor(1, 0);
    OK(ghostty_render_state_update(render, terminal));
    dirty_state(GHOSTTY_RENDER_STATE_DIRTY_PARTIAL);
    dirty_rows(true, false);
    render_text(0, 0, "B");
    dirty_rows(false, true);
    OK(ghostty_render_state_set(render, GHOSTTY_RENDER_STATE_OPTION_DIRTY, &clean));
    OK(ghostty_render_state_update(render, terminal));
    dirty_state(GHOSTTY_RENDER_STATE_DIRTY_FALSE);
    dirty_rows(false, false);
    puts("CASE dirty-updates OK: FULL -> acknowledged clean -> PARTIAL(row0 only) -> acknowledged clean; snapshot B");
}

static void key(const char *name, GhosttyKey physical, GhosttyMods mods,
                const char *text, uint32_t unshifted, const char *expected)
{
    char output[128];
    size_t len = 0;
    ghostty_key_encoder_setopt_from_terminal(encoder, terminal);
    ghostty_key_event_set_action(event, GHOSTTY_KEY_ACTION_PRESS);
    ghostty_key_event_set_key(event, physical);
    ghostty_key_event_set_mods(event, mods);
    ghostty_key_event_set_consumed_mods(event, 0);
    ghostty_key_event_set_composing(event, false);
    ghostty_key_event_set_utf8(event, text, text ? strlen(text) : 0);
    ghostty_key_event_set_unshifted_codepoint(event, unshifted);
    OK(ghostty_key_encoder_encode(encoder, event, output, sizeof(output), &len));
    equal("key output fits buffer", len <= sizeof(output), true);
    bytes_equal(name, output, len, expected, strlen(expected));
    printf("CASE key/%s OK bytes=", name);
    hex(stdout, output, len);
    fputc('\n', stdout);
}

static void keys(void)
{
    case_name = "key-encoding";
    ghostty_terminal_reset(terminal);
    FEED("\x1b[?1l\x1b>");
    key("normal-up", GHOSTTY_KEY_ARROW_UP, 0, NULL, 0, "\x1b[A");
    key("normal-left", GHOSTTY_KEY_ARROW_LEFT, 0, NULL, 0, "\x1b[D");
    key("numeric-keypad1", GHOSTTY_KEY_NUMPAD_1, 0, "1", '1', "1");
    FEED("\x1b[?1h\x1b=\x1b[?1035h");
    mode(GHOSTTY_MODE_KEYPAD_KEYS, true);
    key("application-up", GHOSTTY_KEY_ARROW_UP, 0, NULL, 0, "\x1bOA");
    key("application-left", GHOSTTY_KEY_ARROW_LEFT, 0, NULL, 0, "\x1bOD");
    /* This pin's DEC1035 overrides application keypad mode even without
     * a NumLock event flag. Exercise the precedence, not a default guess. */
    mode(GHOSTTY_MODE_NUMLOCK_KEYPAD, true);
    key("keypad-1035-override", GHOSTTY_KEY_NUMPAD_1, 0, "1", '1', "1");
    FEED("\x1b[?1035l");
    mode(GHOSTTY_MODE_NUMLOCK_KEYPAD, false);
    key("application-keypad1", GHOSTTY_KEY_NUMPAD_1, 0, "1", '1', "\x1bOq");
    key("application-keypad-enter", GHOSTTY_KEY_NUMPAD_ENTER, 0, NULL, 0, "\x1bOM");
    key("shift-up", GHOSTTY_KEY_ARROW_UP, GHOSTTY_MODS_SHIFT, NULL, 0, "\x1b[1;2A");
    key("ctrl-up", GHOSTTY_KEY_ARROW_UP, GHOSTTY_MODS_CTRL, NULL, 0, "\x1b[1;5A");
    key("alt-left", GHOSTTY_KEY_ARROW_LEFT, GHOSTTY_MODS_ALT, NULL, 0, "\x1b[1;3D");
    key("ctrl-c", GHOSTTY_KEY_C, GHOSTTY_MODS_CTRL, "c", 'c', "\x03");
    FEED("\x1b[?1l\x1b>");
    mode(GHOSTTY_MODE_KEYPAD_KEYS, false);
    key("restored-normal-up", GHOSTTY_KEY_ARROW_UP, 0, NULL, 0, "\x1b[A");
    key("restored-numeric-keypad1", GHOSTTY_KEY_NUMPAD_1, 0, "1", '1', "1");
}

static void paste(bool bracketed)
{
    char input[] = "one\ntwo";
    char output[128];
    size_t len = 0;
    bool active = mode(GHOSTTY_MODE_BRACKETED_PASTE, bracketed);
    OK(ghostty_paste_encode(input, sizeof(input) - 1, active,
                           output, sizeof(output), &len));
    equal("paste output fits buffer", len <= sizeof(output), true);
    const char *expected = bracketed ? "\x1b[200~one\ntwo\x1b[201~" : "one\rtwo";
    bytes_equal("paste encoded bytes", output, len, expected, strlen(expected));
    printf("CASE paste/%s OK bytes=", bracketed ? "on" : "off");
    hex(stdout, output, len);
    fputc('\n', stdout);
}

int main(void)
{
    struct timespec start, end;
    struct rusage usage;
    if (clock_gettime(CLOCK_MONOTONIC, &start) != 0) {
        perror("FAIL clock_gettime start");
        return EXIT_FAILURE;
    }
    if (atexit(cleanup) != 0) {
        fputs("FAIL atexit expected=0 actual=nonzero\n", stderr);
        return EXIT_FAILURE;
    }
    puts("PROBE headless libghostty-vt pin=c5a21edfcbc2d5b46540ad91b7980aca31f5f1f3; not rendering proof");
    GhosttyTerminalOptions options = { .cols = COLS, .rows = ROWS, .max_scrollback = 100 };
    OK(ghostty_terminal_new(NULL, &terminal, options));
    OK(ghostty_render_state_new(NULL, &render));
    OK(ghostty_render_state_row_iterator_new(NULL, &rows));
    OK(ghostty_render_state_row_cells_new(NULL, &cells));
    OK(ghostty_key_encoder_new(NULL, &encoder));
    OK(ghostty_key_event_new(NULL, &event));
    split_and_unicode();
    hyperlinks();
    alternate_screen();
    styles();
    dirty_updates();
    keys();
    case_name = "paste";
    FEED("\x1b[?2004l");
    paste(false);
    FEED("\x1b[?2004h");
    paste(true);
    FEED("\x1b[?2004l");
    paste(false);
    if (clock_gettime(CLOCK_MONOTONIC, &end) != 0) {
        perror("FAIL clock_gettime end");
        return EXIT_FAILURE;
    }
    if (getrusage(RUSAGE_SELF, &usage) != 0) {
        perror("FAIL getrusage");
        return EXIT_FAILURE;
    }
    double elapsed_ms = (double)(end.tv_sec - start.tv_sec) * 1000.0
                      + (double)(end.tv_nsec - start.tv_nsec) / 1000000.0;
    printf("METRICS elapsed_monotonic_ms=%.3f maxrss_kib=%ld (Linux ru_maxrss; includes startup; not a workload benchmark)\n",
           elapsed_ms, usage.ru_maxrss);
    printf("PASS all native feasibility cases; checks=%u\n", checks);
    return EXIT_SUCCESS;
}
