/** Invite code generation. */

/** Length of an invite code. */
export const INVITE_CODE_LENGTH = 10;

/** Alphabet of invite codes: `A-Z` and `2-9` (no `0`/`1`, which are easy to misread). */
export const INVITE_ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZ23456789";

/** How long an invite stays valid: 24 hours. */
export const INVITE_TTL_MS = 24 * 60 * 60 * 1000;

/** How many devices one invite admits. */
export const INVITE_USES = 5;

/** Source of random bytes (injected so tests are deterministic). */
export type RandomBytes = (length: number) => Uint8Array;

/**
 * Builds a random invite code of {@link INVITE_CODE_LENGTH} characters from
 * {@link INVITE_ALPHABET}.
 *
 * Uses rejection sampling: bytes at or above the largest multiple of the alphabet size are
 * discarded, so every character is exactly equally likely.
 */
export function generateInviteCode(randomBytes: RandomBytes): string {
  const size = INVITE_ALPHABET.length;
  const limit = 256 - (256 % size);
  let code = "";
  while (code.length < INVITE_CODE_LENGTH) {
    for (const byte of randomBytes(INVITE_CODE_LENGTH * 2)) {
      if (byte < limit && code.length < INVITE_CODE_LENGTH) {
        code += INVITE_ALPHABET.charAt(byte % size);
      }
    }
  }
  return code;
}
