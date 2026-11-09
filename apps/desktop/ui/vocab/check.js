// Client-side mirror of `v2c_core::vocab::check` so the editor can show problems while typing.
// The backend validates again on save; limits come from `vocab_get`.

/**
 * @param {string} text editor text, one entry per line
 * @param {{ maxEntries: number, maxEntryChars: number }} limits
 * @returns {{ count: number, tooLongLines: number[], overLimit: number }}
 */
export function checkVocab(text, { maxEntries, maxEntryChars }) {
  const seen = new Set();
  const tooLongLines = [];
  let count = 0;
  let overLimit = 0;
  text
    .replace(/^﻿/, "")
    .split(/\r?\n/)
    .forEach((raw, i) => {
      const line = raw.trim();
      if (line === "" || line.startsWith("#")) return;
      const word = line.split(/\s+/).join(" ");
      // Count code points, like Rust's `chars().count()`.
      if ([...word].length > maxEntryChars) {
        tooLongLines.push(i + 1);
        return;
      }
      const key = word.toLowerCase();
      if (seen.has(key)) return;
      seen.add(key);
      if (count < maxEntries) count += 1;
      else overLimit += 1;
    });
  return { count, tooLongLines, overLimit };
}
