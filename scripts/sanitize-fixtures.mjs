import { createHash } from "node:crypto";
import { readFile, readdir, rename, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { normalizeLabel } from "../dist/labels.js";

const HELP = `Usage: node scripts/sanitize-fixtures.mjs [options]

Rewrites the recorded UIA observations in tests/fixtures/uia so that they keep
every structural property the filter tests measure and carry none of the text
that was on the screen when they were recorded.

Replaced: element name, value, automationId and className, plus window.title and
window.app. The accessibility tree string is rebuilt from the replacements in the
shape uia.cpp emits. Everything else -- index, controlType, patterns, bounds,
enabled, offscreen, focused, runtimeId, elementCount, element order, the window
rectangle and its flags, and whether an element carries a value field at all --
is copied through untouched.

Each recording is also renamed to app-NN-asfound.json / app-NN-foreground.json so
the filenames stop naming real applications. A file already carrying that name is
left alone, so a second run changes nothing.

Run npm run build first: the replacement has to agree with the shared
normalizeLabel, and it is imported from dist rather than copied.

  --dir <path>   Fixture directory (default tests/fixtures/uia).
  --check        Report what would change and write nothing.
  --help         Show this text.`;

const root = resolve(fileURLToPath(new URL("..", import.meta.url)));

/**
 * A recorded observation is named `<app>.exe[-<window>]-<pass>.json`; the pass
 * is what distinguishes the two recordings of one window.
 */
const RECORDING = /^(.+)-(asfound|foreground)\.json$/;
const SANITIZED = /^app-\d+-(asfound|foreground)\.json$/;

/**
 * The characters that carry what was on the screen, each replaced from within
 * its own script so that Hangul stays Hangul, a digit stays a digit and a
 * Hangul consonant stays a consonant -- NFKC composes a lone consonant and a
 * lone vowel into one syllable, so swapping those two would change a label's
 * length once `normalizeLabel` ran.
 *
 * Everything outside these ranges -- spaces, punctuation, brackets, symbols and
 * control characters -- is copied through, because the filter's text handling
 * keys off exactly those characters: `cleanText` collapses whitespace and
 * control codes, `normalizeLabel` strips ampersands and a trailing ellipsis, and
 * `humanizeAutomationId` splits on `_`, `-` and `.`.
 */
const KATAKANA = "アイウエオカキクケコサシスセソタチツテトナニヌネノハヒフヘホマミムメモヤユヨラリルレロワヲン";

const ALPHABETS = [
  { first: 0x30, last: 0x39, chars: "0123456789" },
  // ASCII letters are drawn as consonant and vowel in turn; see pronounceable().
  { first: 0x61, last: 0x7a, chars: "" },
  { first: 0x3041, last: 0x3096, chars: "あいうえおかきくけこさしすせそたちつてとなにぬねのはひふへほまみむめもやゆよらりるれろわをん" },
  { first: 0x30a1, last: 0x30fa, chars: KATAKANA },
  // The prolonged sound mark reads as a letter, while the middle dot between
  // them at U+30FB is punctuation and has to stay a token separator.
  { first: 0x30fc, last: 0x30fc, chars: KATAKANA },
  { first: 0x3131, last: 0x314e, chars: "ㄱㄴㄷㄹㅁㅂㅅㅇㅈㅊㅋㅌㅍㅎ" },
  { first: 0x314f, last: 0x3163, chars: "ㅏㅑㅓㅕㅗㅛㅜㅠㅡㅣ" },
  { first: 0xac00, last: 0xd7a3, chars: "가나다라마바사아자차카타파하고노도로모보소오조초코토포호구누두루무부수우주추쿠투푸후그느드르므브스으즈츠크트프흐" },
  { first: 0x4e00, last: 0x9fff, chars: "一二三四五六七八九十上下左右大小中山川水火木金土日月田石目耳手足人力口" }
];

const CONSONANTS = "bdfghjklmnprstvz";
const VOWELS = "aeiou";

/** A value the daemon already withheld; it is a marker, not screen text. */
const REDACTED = "[redacted]";

function parseArgs(argv) {
  const options = { dir: join(root, "tests", "fixtures", "uia"), check: false, help: false };
  for (let i = 0; i < argv.length; i += 1) {
    const equals = argv[i].indexOf("=");
    const name = equals >= 0 ? argv[i].slice(0, equals) : argv[i];
    const inline = equals >= 0 ? argv[i].slice(equals + 1) : undefined;
    if (name === "--help" || name === "-h") options.help = true;
    else if (name === "--check") options.check = true;
    else if (name === "--dir") {
      const raw = inline ?? argv[++i];
      if (!raw) throw Error("--dir requires a value");
      options.dir = resolve(raw);
    } else throw Error(`Unknown option ${name}`);
  }
  return options;
}

function alphabetOf(codePoint) {
  return ALPHABETS.find(alphabet => codePoint >= alphabet.first && codePoint <= alphabet.last);
}

/** The alphabet a character is replaced from, found through its lower case for ASCII. */
function alphabetFor(character) {
  return alphabetOf(character.codePointAt(0)) ?? alphabetOf(character.toLowerCase().codePointAt(0));
}

/**
 * The seed for a string is its replaceable content alone, case-folded. Two
 * spellings the filter already reads as one label -- "SaveButton" against
 * "Save Button", "UpButton" against "upButton", "Save..." against "Save" --
 * therefore draw the same replacement characters, which is what keeps
 * `normalizeLabel` equality, the parent/child collapse buckets, the deduplication
 * and the automation-id alt-label rule intact.
 */
function seedOf(text) {
  let seed = "";
  for (const character of text) {
    if (alphabetFor(character)) seed += character.toLowerCase();
  }
  return seed;
}

/** A deterministic byte stream, long enough for the longest recorded value. */
function streamFor(seed, salt, count) {
  const bytes = [];
  for (let block = 0; bytes.length < count * 4; block += 1) {
    bytes.push(...createHash("sha256").update(`uia-fixture/${salt}/${block}/${seed}`).digest());
  }
  return position => {
    const at = position * 4;
    return ((bytes[at] << 24) >>> 0) + (bytes[at + 1] << 16) + (bytes[at + 2] << 8) + bytes[at + 3];
  };
}

/** Consonants and vowels in turn, so a replaced label still reads as words. */
function pronounceable(draw, position) {
  const set = position % 2 === 0 ? CONSONANTS : VOWELS;
  return set[draw % set.length];
}

function replaceText(text, salt) {
  if (text === "" || text === REDACTED) return text;
  const seed = seedOf(text);
  // Punctuation alone -- ", ", a newline, a run of middle dots -- says nothing
  // about the screen it came from and has nothing to replace it with.
  if (seed === "") return text;
  const characters = [...text];
  const draw = streamFor(seed, salt, characters.length);
  let out = "";
  let position = 0;
  for (const character of characters) {
    const alphabet = alphabetFor(character);
    if (!alphabet) {
      out += character;
      continue;
    }
    const value = draw(position);
    const replacement = alphabet.chars === ""
      ? pronounceable(value, position)
      : alphabet.chars[value % alphabet.chars.length];
    out += character === character.toLowerCase() ? replacement : replacement.toUpperCase();
    position += 1;
  }
  return out;
}

/** The line shape `uia.cpp` appends per element, rebuilt from the sanitized text. */
function treeLine(element) {
  const bounds = element.bounds ?? {};
  let line = `[${element.index}] ${element.controlType}`;
  if (element.name) line += ` "${element.name}"`;
  line += ` rect=${bounds.x},${bounds.y} ${bounds.width}x${bounds.height}`;
  if (element.value === REDACTED) line += " value=[redacted]";
  else if (element.value) line += ` value="${element.value}"`;
  return `${line}\n`;
}

function buildTree(elements) {
  return elements.map(treeLine).join("");
}

const TEXT_FIELDS = ["name", "value", "automationId", "className"];

function eachString(document, visit) {
  visit(document.window, "title");
  visit(document.window, "app");
  for (const element of document.accessibility?.elements ?? []) {
    for (const field of TEXT_FIELDS) visit(element, field);
  }
}

function sanitizeDocument(document, replacementOf) {
  eachString(document, (holder, field) => {
    if (holder && typeof holder[field] === "string") holder[field] = replacementOf(holder[field]);
  });
  if (typeof document.accessibility?.tree === "string") {
    document.accessibility.tree = buildTree(document.accessibility.elements ?? []);
  }
}

function collectStrings(document, into) {
  eachString(document, (holder, field) => {
    const text = holder?.[field];
    if (typeof text === "string" && text !== "" && text !== REDACTED) into.add(text);
  });
}

/**
 * Every letter and digit has to land in one of the alphabets above; a script
 * with no alphabet would otherwise be copied through as screen text.
 */
function unhandledLetters(document, into) {
  eachString(document, (holder, field) => {
    const text = holder?.[field];
    if (typeof text !== "string") return;
    for (const character of text) {
      if (alphabetFor(character)) continue;
      if (/[\p{L}\p{N}]/u.test(character)) into.add(`${character} U+${character.codePointAt(0).toString(16)}`);
    }
  });
}

/**
 * A seed with only a handful of characters has only a handful of replacements to
 * draw from -- ten single-digit names against a ten-digit alphabet -- so two of
 * them will land on the same text. Every string that shares a seed is redrawn
 * together with the next salt until nothing it produces reads as a label some
 * other seed already produced, which is what the collapse and dedupe buckets and
 * the label-against-value comparison need.
 */
function planReplacements(strings) {
  const bySeed = new Map();
  for (const text of [...strings].sort()) {
    const seed = seedOf(text);
    const group = bySeed.get(seed);
    if (group) group.push(text);
    else bySeed.set(seed, [text]);
  }
  const claimedBy = new Map();
  const replacement = new Map();
  let redraws = 0;
  for (const [seed, group] of [...bySeed].sort(([left], [right]) => (left < right ? -1 : left > right ? 1 : 0))) {
    for (let salt = 0; ; salt += 1) {
      const drawn = group.map(text => [text, replaceText(text, salt)]);
      const claims = drawn.map(([text, out]) => [normalizeLabel(out), normalizeLabel(text)]);
      const conflict = claims.some(([out, source], position) =>
        (claimedBy.has(out) && claimedBy.get(out) !== source) ||
        claims.some(([otherOut, otherSource], other) => other !== position && otherOut === out && otherSource !== source));
      if (conflict) {
        redraws += 1;
        if (salt > 1000) throw Error(`No salt gave ${JSON.stringify(seed)} a free replacement`);
        continue;
      }
      for (const [out, source] of claims) claimedBy.set(out, source);
      for (const [text, out] of drawn) replacement.set(text, out);
      break;
    }
  }
  return { replacement, redraws };
}

const options = parseArgs(process.argv.slice(2));
if (options.help) {
  console.log(HELP);
  process.exit(0);
}

const entries = (await readdir(options.dir)).filter(name => name.endsWith(".json")).sort();
const pending = entries.filter(name => RECORDING.test(name) && !SANITIZED.test(name));
if (pending.length === 0) {
  console.log(`Nothing to do: every recording in ${options.dir} is already sanitized.`);
  process.exit(0);
}

// One number per window, so the two passes of a window keep their pairing.
const stems = [...new Set(pending.map(name => name.match(RECORDING)[1]))].sort();
const numberOf = new Map(stems.map((stem, position) => [stem, String(position + 1).padStart(2, "0")]));

const before = new Set();
const unhandled = new Set();
const read = [];

for (const name of pending) {
  const [, stem, pass] = name.match(RECORDING);
  const document = JSON.parse(await readFile(join(options.dir, name), "utf8"));
  collectStrings(document, before);
  unhandledLetters(document, unhandled);
  const elements = document.accessibility?.elements ?? [];
  if (typeof document.accessibility?.tree === "string" && document.accessibility.tree !== buildTree(elements)) {
    throw Error(`${name}: the recorded tree is not what its elements describe, so it cannot be rebuilt`);
  }
  read.push({ name, renamed: `app-${numberOf.get(stem)}-${pass}.json`, document });
}

if (unhandled.size > 0) {
  throw Error(`No alphabet covers these characters, so they would have survived: ${[...unhandled].join(" ")}`);
}

const { replacement, redraws } = planReplacements(before);
const after = new Set(replacement.values());
// Text that differed has to keep differing, or the collapse and dedupe buckets would merge.
if (after.size !== before.size) {
  throw Error(`Two strings that differed were replaced by the same one: ${before.size} in, ${after.size} out`);
}

for (const { name, renamed, document } of read) {
  sanitizeDocument(document, text => replacement.get(text) ?? text);
  console.log(`${name} -> ${renamed}`);
  if (options.check) continue;
  await writeFile(join(options.dir, name), `${JSON.stringify(document, null, 2)}\n`, "utf8");
  await rename(join(options.dir, name), join(options.dir, renamed));
}
console.log(`${read.length} recordings ${options.check ? "would be " : ""}sanitized, ` +
  `${before.size} distinct strings replaced, ${redraws} redrawn to stay distinct.`);
