// Sample data with implicit references (xxxId fields) for schema inference.
const shop = db.getSiblingDB('shop');
const annId = ObjectId(), bobId = ObjectId(), duneId = ObjectId(), chessId = ObjectId();
shop.customers.insertMany([
  { _id: annId, email: 'ann@example.com', name: 'Ann Lee', vip: true, address: { city: 'Tokyo', zip: '100-0001' }, createdAt: new Date() },
  { _id: bobId, email: 'bob@example.com', name: 'Bob Ito', tags: ['new'], createdAt: new Date() },
]);
shop.products.insertMany([
  { _id: duneId, sku: 'BK000001', name: 'Dune', price: NumberDecimal('9.99'), categories: ['books', 'scifi'] },
  { _id: chessId, sku: 'GM000001', name: 'Chess', price: NumberDecimal('19.50') },
]);
shop.orders.insertMany([
  { customerId: annId, status: 'paid', items: [ { productId: duneId, qty: 1 }, { productId: chessId, qty: 1 } ], total: 29.49, orderedAt: new Date() },
  { customerId: bobId, status: 'new', items: [ { productId: duneId, qty: 3 } ], orderedAt: new Date() },
]);
shop.reviews.insertOne({ product_id: duneId, customer: { $ref: 'customers', $id: annId }, stars: 5, text: 'Great' });
shop.customers.createIndex({ email: 1 }, { unique: true });
shop.orders.createIndex({ customerId: 1, orderedAt: -1 });
const bulk = [];
for (let i = 0; i < 5000; i++) bulk.push({ n: i, payload: 'event ' + i });
shop.events.insertMany(bulk);
