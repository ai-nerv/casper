/*
 * casper's grammar driver: one source file on stdin, its syntax tree on stdout.
 *
 * Built once per language by `casper grammar build`, linked against that grammar and the
 * tree-sitter runtime, and run by `casper code` inside its jail. It reads nothing but stdin and
 * writes nothing but stdout and one line of stderr when it refuses.
 *
 * Output, little-endian throughout:
 *
 *   "CSTR" u32 version
 *   u16 length + bytes      the grammar revision it was built from
 *   u32 S, then S+1 names   symbol names by id; name S is "ERROR", for the error symbol
 *   u32 F, then F names     field names for ids 1..F
 *   u32 N, then N records   nodes in pre-order, RECORD bytes each:
 *     u16 kind  u16 field  u16 flags  u16 zero
 *     u32 start_byte  u32 end_byte  u32 start_row  u32 start_col  u32 end_row  u32 end_col
 *     u32 child_count
 *
 * A name is a u16 length and that many bytes.
 */

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <unistd.h>

#include <tree_sitter/api.h>

#ifndef CASPER_LANGUAGE
#error "CASPER_LANGUAGE names the grammar's tree_sitter_<name> function"
#endif
#ifndef CASPER_REV
#define CASPER_REV "unknown"
#endif

const TSLanguage *CASPER_LANGUAGE(void);

#define VERSION 1u
#define MOST_INPUT (2u * 1024u * 1024u)
#define MOST_NODES (1u << 20)
#define MOST_OUTPUT (48u * 1024u * 1024u)
#define MOST_MEMORY ((rlim_t)1 << 30)

enum {
  TOO_LARGE = 2,
  TOO_MANY = 3,
  TOO_LONG = 4,
  UNREAD = 5,
  UNWRITTEN = 6,
  NO_TREE = 7,
  WRONG_ABI = 8,
};

enum {
  NAMED = 1u << 0,
  EXTRA = 1u << 1,
  HAS_ERROR = 1u << 2,
  MISSING = 1u << 3,
};

static uint8_t out[1u << 16];
static size_t held;
static size_t written;

static void quit(int code, const char *why) {
  fputs(why, stderr);
  fputc('\n', stderr);
  _exit(code);
}

static void flush(void) {
  size_t at = 0;
  while (at < held) {
    ssize_t n = write(STDOUT_FILENO, out + at, held - at);
    if (n < 0 && errno == EINTR) continue;
    if (n <= 0) quit(UNWRITTEN, "the tree could not be written");
    at += (size_t)n;
  }
  held = 0;
}

static void put(const void *bytes, size_t n) {
  const uint8_t *from = bytes;
  if (n > MOST_OUTPUT - written) quit(TOO_LONG, "the tree is larger than its output cap");
  written += n;
  while (n > 0) {
    size_t room = sizeof out - held;
    size_t take = n < room ? n : room;
    memcpy(out + held, from, take);
    held += take;
    from += take;
    n -= take;
    if (held == sizeof out) flush();
  }
}

static void u16le(uint32_t v) {
  uint8_t b[2] = {(uint8_t)v, (uint8_t)(v >> 8)};
  put(b, sizeof b);
}

static void u32le(uint32_t v) {
  uint8_t b[4] = {(uint8_t)v, (uint8_t)(v >> 8), (uint8_t)(v >> 16), (uint8_t)(v >> 24)};
  put(b, sizeof b);
}

static void name(const char *text) {
  size_t n = text ? strlen(text) : 0;
  if (n > 0xffffu) n = 0xffffu;
  u16le((uint32_t)n);
  put(text, n);
}

/* All of stdin, refused past MOST_INPUT. */
static char *source(uint32_t *length) {
  char *bytes = malloc(MOST_INPUT + 1u);
  size_t got = 0;
  if (!bytes) quit(UNREAD, "no memory for the source");
  for (;;) {
    ssize_t n = read(STDIN_FILENO, bytes + got, MOST_INPUT + 1u - got);
    if (n < 0 && errno == EINTR) continue;
    if (n < 0) quit(UNREAD, "the source could not be read");
    if (n == 0) break;
    got += (size_t)n;
    if (got > MOST_INPUT) quit(TOO_LARGE, "the source is larger than 2 MiB");
  }
  *length = (uint32_t)got;
  return bytes;
}

static void record(TSTreeCursor *cursor, uint32_t symbols) {
  TSNode node = ts_tree_cursor_current_node(cursor);
  TSSymbol kind = ts_node_symbol(node);
  TSPoint start = ts_node_start_point(node), end = ts_node_end_point(node);
  uint32_t flags = (ts_node_is_named(node) ? NAMED : 0) | (ts_node_is_extra(node) ? EXTRA : 0) |
                   (ts_node_has_error(node) ? HAS_ERROR : 0) |
                   (ts_node_is_missing(node) ? MISSING : 0);
  u16le(kind < symbols ? kind : symbols);
  u16le(ts_tree_cursor_current_field_id(cursor));
  u16le(flags);
  u16le(0);
  u32le(ts_node_start_byte(node));
  u32le(ts_node_end_byte(node));
  u32le(start.row);
  u32le(start.column);
  u32le(end.row);
  u32le(end.column);
  u32le(ts_node_child_count(node));
}

/* Visit every node in pre-order: count them when `symbols` is zero, else write each. */
static uint32_t walk(TSNode root, uint32_t symbols) {
  TSTreeCursor cursor = ts_tree_cursor_new(root);
  uint32_t seen = 0;
  for (;;) {
    if (++seen > MOST_NODES) quit(TOO_MANY, "the tree has more nodes than its cap");
    if (symbols) record(&cursor, symbols);
    if (ts_tree_cursor_goto_first_child(&cursor)) continue;
    while (!ts_tree_cursor_goto_next_sibling(&cursor)) {
      if (!ts_tree_cursor_goto_parent(&cursor)) {
        ts_tree_cursor_delete(&cursor);
        return seen;
      }
    }
  }
}

int main(int argc, char **argv) {
  struct rlimit memory;
  if (getrlimit(RLIMIT_AS, &memory) == 0 && memory.rlim_cur > MOST_MEMORY) {
    memory.rlim_cur = MOST_MEMORY;
    setrlimit(RLIMIT_AS, &memory);
  }
  if (argc > 1 && strcmp(argv[1], "--rev") == 0) {
    puts(CASPER_REV);
    return 0;
  }

  uint32_t length = 0;
  char *text = source(&length);
  const TSLanguage *language = CASPER_LANGUAGE();
  TSParser *parser = ts_parser_new();
  if (!ts_parser_set_language(parser, language)) quit(WRONG_ABI, "the grammar's ABI is not this runtime's");
  TSTree *tree = ts_parser_parse_string(parser, NULL, text, length);
  if (!tree) quit(NO_TREE, "the parser returned no tree");
  TSNode root = ts_tree_root_node(tree);
  uint32_t nodes = walk(root, 0);

  uint32_t symbols = ts_language_symbol_count(language);
  uint32_t fields = ts_language_field_count(language);
  if (symbols > 0xfffeu) quit(TOO_MANY, "the grammar has more symbols than the format holds");
  put("CSTR", 4);
  u32le(VERSION);
  name(CASPER_REV);
  u32le(symbols);
  for (uint32_t id = 0; id < symbols; id++) name(ts_language_symbol_name(language, (TSSymbol)id));
  name("ERROR");
  u32le(fields);
  for (uint32_t id = 1; id <= fields; id++) name(ts_language_field_name_for_id(language, (TSFieldId)id));
  u32le(nodes);
  walk(root, symbols);
  flush();
  return 0;
}
