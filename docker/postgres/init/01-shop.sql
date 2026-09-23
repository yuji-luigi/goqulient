-- Sample schema used by integration tests and ER reverse-engineering.
CREATE TABLE categories (
  id         serial PRIMARY KEY,
  name       varchar(100) NOT NULL UNIQUE,
  parent_id  integer REFERENCES categories(id) ON DELETE SET NULL
);
CREATE TABLE customers (
  id         bigserial PRIMARY KEY,
  email      varchar(255) NOT NULL UNIQUE,
  full_name  text NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now(),
  profile    jsonb
);
COMMENT ON TABLE customers IS 'People who buy things';
CREATE TABLE products (
  id          serial PRIMARY KEY,
  sku         char(8) NOT NULL UNIQUE,
  name        text NOT NULL,
  price       numeric(10,2) NOT NULL CHECK (price >= 0),
  category_id integer REFERENCES categories(id),
  tags        text[],
  uid         uuid DEFAULT gen_random_uuid()
);
CREATE TABLE orders (
  id          bigserial PRIMARY KEY,
  customer_id bigint NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
  status      varchar(20) NOT NULL DEFAULT 'new',
  ordered_on  date NOT NULL DEFAULT current_date,
  total       numeric(12,2)
);
CREATE INDEX orders_customer_idx ON orders(customer_id);
CREATE TABLE order_items (
  order_id   bigint  NOT NULL REFERENCES orders(id) ON DELETE CASCADE,
  line_no    integer NOT NULL,
  product_id integer NOT NULL REFERENCES products(id),
  qty        integer NOT NULL DEFAULT 1,
  PRIMARY KEY (order_id, line_no)
);
-- composite FK target
CREATE TABLE shipments (
  id       serial PRIMARY KEY,
  order_id bigint  NOT NULL,
  line_no  integer NOT NULL,
  shipped  boolean NOT NULL DEFAULT false,
  FOREIGN KEY (order_id, line_no) REFERENCES order_items(order_id, line_no)
);
-- many-to-many
CREATE TABLE product_favorites (
  customer_id bigint  REFERENCES customers(id),
  product_id  integer REFERENCES products(id),
  PRIMARY KEY (customer_id, product_id)
);
CREATE SCHEMA inventory;
CREATE TABLE inventory.warehouses (id serial PRIMARY KEY, city text NOT NULL);
CREATE TABLE inventory.stock (
  warehouse_id integer REFERENCES inventory.warehouses(id),
  product_id   integer REFERENCES public.products(id),
  qty          integer NOT NULL DEFAULT 0,
  PRIMARY KEY (warehouse_id, product_id)
);
CREATE VIEW order_totals AS
  SELECT o.id, o.customer_id, sum(p.price * i.qty) AS total
  FROM orders o JOIN order_items i ON i.order_id = o.id JOIN products p ON p.id = i.product_id
  GROUP BY o.id;

INSERT INTO categories(name) VALUES ('Books'),('Games');
INSERT INTO categories(name,parent_id) VALUES ('Sci-Fi',1);
INSERT INTO customers(email,full_name,profile) VALUES
 ('ann@example.com','Ann Lee','{"vip":true}'),('bob@example.com','Bob Ito',NULL);
INSERT INTO products(sku,name,price,category_id,tags) VALUES
 ('BK000001','Dune',9.99,3,'{scifi,classic}'),('GM000001','Chess',19.50,2,NULL);
INSERT INTO orders(customer_id,status,total) VALUES (1,'paid',29.49),(2,'new',NULL);
INSERT INTO order_items VALUES (1,1,1,1),(1,2,2,1),(2,1,1,3);
INSERT INTO shipments(order_id,line_no,shipped) VALUES (1,1,true);
INSERT INTO inventory.warehouses(city) VALUES ('Tokyo');
INSERT INTO inventory.stock VALUES (1,1,10),(1,2,4);
-- a bigger table to test row limits / paging
CREATE TABLE events (id bigserial PRIMARY KEY, payload text, at timestamptz DEFAULT now());
INSERT INTO events(payload) SELECT 'event ' || g FROM generate_series(1, 20000) g;
