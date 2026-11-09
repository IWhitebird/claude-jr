const products = [
  { id: 1, name: "Apple", emoji: "🍎", price: 2.99, unit: "per lb", cat: "classic" },
  { id: 2, name: "Banana", emoji: "🍌", price: 0.69, unit: "per lb", cat: "tropical" },
  { id: 3, name: "Orange", emoji: "🍊", price: 1.49, unit: "each", cat: "citrus" },
  { id: 4, name: "Lemon", emoji: "🍋", price: 0.79, unit: "each", cat: "citrus" },
  { id: 5, name: "Strawberries", emoji: "🍓", price: 4.49, unit: "per box", cat: "berries" },
  { id: 6, name: "Blueberries", emoji: "🫐", price: 5.29, unit: "per box", cat: "berries" },
  { id: 7, name: "Grapes", emoji: "🍇", price: 3.99, unit: "per lb", cat: "classic" },
  { id: 8, name: "Pineapple", emoji: "🍍", price: 3.49, unit: "each", cat: "tropical" },
  { id: 9, name: "Mango", emoji: "🥭", price: 1.99, unit: "each", cat: "tropical" },
  { id: 10, name: "Watermelon", emoji: "🍉", price: 6.99, unit: "each", cat: "classic" },
  { id: 11, name: "Peach", emoji: "🍑", price: 2.49, unit: "per lb", cat: "classic" },
  { id: 12, name: "Kiwi", emoji: "🥝", price: 0.59, unit: "each", cat: "tropical" },
  { id: 13, name: "Cherries", emoji: "🍒", price: 6.49, unit: "per lb", cat: "berries" },
  { id: 14, name: "Pear", emoji: "🍐", price: 2.29, unit: "per lb", cat: "classic" },
  { id: 15, name: "Coconut", emoji: "🥥", price: 3.29, unit: "each", cat: "tropical" },
  { id: 16, name: "Grapefruit", emoji: "🍊", price: 1.89, unit: "each", cat: "citrus" },
];

const $ = (sel) => document.querySelector(sel);
const grid = $("#productGrid");
const search = $("#search");
const noResults = $("#noResults");
const cartEl = $("#cart");
const overlay = $("#overlay");
const cartItemsEl = $("#cartItems");
const cartCount = $("#cartCount");
const cartTotal = $("#cartTotal");
const toast = $("#toast");

let activeCat = "all";
let cart = JSON.parse(localStorage.getItem("fruitCart") || "{}"); // { id: qty }

const money = (n) => `$${n.toFixed(2)}`;

/* ---------- Products ---------- */
function renderProducts() {
  const q = search.value.trim().toLowerCase();
  const list = products.filter(
    (p) => (activeCat === "all" || p.cat === activeCat) && p.name.toLowerCase().includes(q)
  );

  grid.innerHTML = list
    .map(
      (p) => `
      <div class="card">
        <div class="emoji">${p.emoji}</div>
        <h3>${p.name}</h3>
        <div class="unit">${p.unit}</div>
        <div class="price">${money(p.price)}</div>
        <button data-add="${p.id}">Add to Cart</button>
      </div>`
    )
    .join("");

  noResults.hidden = list.length > 0;
}

grid.addEventListener("click", (e) => {
  const id = e.target.dataset.add;
  if (id) addToCart(Number(id));
});

search.addEventListener("input", renderProducts);

$("#filters").addEventListener("click", (e) => {
  if (!e.target.matches(".filter")) return;
  document.querySelectorAll(".filter").forEach((b) => b.classList.remove("active"));
  e.target.classList.add("active");
  activeCat = e.target.dataset.cat;
  renderProducts();
});

/* ---------- Cart ---------- */
function saveCart() {
  localStorage.setItem("fruitCart", JSON.stringify(cart));
}

function addToCart(id) {
  cart[id] = (cart[id] || 0) + 1;
  saveCart();
  renderCart();
  const p = products.find((x) => x.id === id);
  showToast(`${p.emoji} ${p.name} added to cart`);
}

function changeQty(id, delta) {
  cart[id] = (cart[id] || 0) + delta;
  if (cart[id] <= 0) delete cart[id];
  saveCart();
  renderCart();
}

function renderCart() {
  const entries = Object.entries(cart);
  let total = 0;
  let count = 0;

  if (entries.length === 0) {
    cartItemsEl.innerHTML = `<li class="empty">Your cart is empty 🧺</li>`;
  } else {
    cartItemsEl.innerHTML = entries
      .map(([id, qty]) => {
        const p = products.find((x) => x.id === Number(id));
        if (!p) return "";
        total += p.price * qty;
        count += qty;
        return `
        <li class="cart-item">
          <span class="emoji">${p.emoji}</span>
          <div class="info">
            <div>${p.name}</div>
            <small>${money(p.price)} ${p.unit}</small>
          </div>
          <div class="qty">
            <button data-dec="${p.id}">−</button>
            <span>${qty}</span>
            <button data-inc="${p.id}">+</button>
          </div>
          <button class="remove" data-remove="${p.id}" aria-label="Remove">🗑</button>
        </li>`;
      })
      .join("");
  }

  cartCount.textContent = count;
  cartTotal.textContent = money(total);
}

cartItemsEl.addEventListener("click", (e) => {
  const { inc, dec, remove } = e.target.dataset;
  if (inc) changeQty(Number(inc), 1);
  if (dec) changeQty(Number(dec), -1);
  if (remove) changeQty(Number(remove), -Infinity);
});

function openCart() {
  cartEl.classList.add("open");
  overlay.classList.add("show");
  cartEl.setAttribute("aria-hidden", "false");
}
function closeCart() {
  cartEl.classList.remove("open");
  overlay.classList.remove("show");
  cartEl.setAttribute("aria-hidden", "true");
}

$("#cartBtn").addEventListener("click", openCart);
$("#closeCart").addEventListener("click", closeCart);
overlay.addEventListener("click", closeCart);
document.addEventListener("keydown", (e) => e.key === "Escape" && closeCart());

$("#checkoutBtn").addEventListener("click", () => {
  if (Object.keys(cart).length === 0) return showToast("Your cart is empty");
  cart = {};
  saveCart();
  renderCart();
  closeCart();
  showToast("🎉 Thanks for your order!");
});

/* ---------- Toast ---------- */
let toastTimer;
function showToast(msg) {
  toast.textContent = msg;
  toast.classList.add("show");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => toast.classList.remove("show"), 2000);
}

/* ---------- Init ---------- */
$("#year").textContent = new Date().getFullYear();
renderProducts();
renderCart();
