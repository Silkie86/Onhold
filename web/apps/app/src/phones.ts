// Phone numbers as written in records, made ready to dial or message.

/** What to dial: the number up to anything that is not part of it ("ext 12", "/ 0770…"). */
export function dialable(phone: string): string {
  const number = phone.split(/[^\d+\s().-]/)[0] ?? "";
  return number.replace(/[^\d+]/g, "");
}
