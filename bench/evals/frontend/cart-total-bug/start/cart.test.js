import assert from "node:assert/strict";
import { total } from "./cart.js";

assert.equal(total([{ price: 999, qty: 2 }]), 1998);
assert.equal(total([{ price: "250", qty: 4 }]), 1000, "prices may arrive as strings");
assert.equal(total([{ price: 1000, qty: 1 }], "SAVE10"), 900);
assert.equal(total([{ price: 333, qty: 1 }], "SAVE10"), 300, "round the final total to whole cents");
assert.equal(total([{ price: 1000, qty: 1 }], "BOGUS"), 1000, "unknown codes are ignored");
assert.equal(total([]), 0);
console.log("ok");
