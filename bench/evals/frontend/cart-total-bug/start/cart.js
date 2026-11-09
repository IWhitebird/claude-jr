// Prices are in cents. A discount code takes a percentage off the whole cart.
const CODES = { SAVE10: 10, HALF: 50 };

export function total(items, code) {
  let sum = 0;
  for (const item of items) {
    let line = item.price * item.qty;
    if (code) line = line - line * (CODES[code] / 100);
    sum += line;
  }
  return sum;
}
