// Phone numbers as written in records, made ready to dial or message.

/** What to dial: the number up to anything that is not part of it ("ext 12", "/ 0770…"). */
export function dialable(phone: string): string {
  const number = phone.split(/[^\d+\s().-]/)[0] ?? "";
  return number.replace(/[^\d+]/g, "");
}

/**
 * A record's numbers to dial: written internationally by the server, unless
 * they no longer line up with the numbers as written (changed offline).
 */
export function phonesOf(c: Record<string, unknown>): string[] {
  const phones = (c.phones as string[] | undefined) ?? [];
  const intl = c.phones_international as string[] | undefined;
  return intl && intl.length === phones.length ? intl : phones;
}
