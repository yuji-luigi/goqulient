CREATE TABLE categories (
  id INT AUTO_INCREMENT PRIMARY KEY,
  name VARCHAR(100) NOT NULL UNIQUE,
  parent_id INT NULL,
  CONSTRAINT fk_cat_parent FOREIGN KEY (parent_id) REFERENCES categories(id) ON DELETE SET NULL
) COMMENT='Product categories';
CREATE TABLE customers (
  id BIGINT AUTO_INCREMENT PRIMARY KEY,
  email VARCHAR(255) NOT NULL UNIQUE,
  full_name VARCHAR(200) NOT NULL,
  created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
  profile JSON NULL
);
CREATE TABLE products (
  id INT AUTO_INCREMENT PRIMARY KEY,
  sku CHAR(8) NOT NULL UNIQUE,
  name VARCHAR(200) NOT NULL,
  price DECIMAL(10,2) NOT NULL,
  category_id INT NULL,
  status ENUM('active','retired') NOT NULL DEFAULT 'active',
  CONSTRAINT fk_prod_cat FOREIGN KEY (category_id) REFERENCES categories(id)
);
CREATE TABLE orders (
  id BIGINT AUTO_INCREMENT PRIMARY KEY,
  customer_id BIGINT NOT NULL,
  status VARCHAR(20) NOT NULL DEFAULT 'new',
  ordered_on DATE NOT NULL,
  total DECIMAL(12,2) NULL,
  KEY orders_customer_idx (customer_id),
  CONSTRAINT fk_order_cust FOREIGN KEY (customer_id) REFERENCES customers(id) ON DELETE CASCADE
);
CREATE TABLE order_items (
  order_id BIGINT NOT NULL,
  line_no INT NOT NULL,
  product_id INT NOT NULL,
  qty INT NOT NULL DEFAULT 1,
  PRIMARY KEY (order_id, line_no),
  CONSTRAINT fk_item_order FOREIGN KEY (order_id) REFERENCES orders(id) ON DELETE CASCADE,
  CONSTRAINT fk_item_prod FOREIGN KEY (product_id) REFERENCES products(id)
);
CREATE TABLE shipments (
  id INT AUTO_INCREMENT PRIMARY KEY,
  order_id BIGINT NOT NULL,
  line_no INT NOT NULL,
  shipped TINYINT(1) NOT NULL DEFAULT 0,
  CONSTRAINT fk_ship_item FOREIGN KEY (order_id, line_no) REFERENCES order_items(order_id, line_no)
);
CREATE VIEW order_totals AS
  SELECT o.id, o.customer_id, SUM(p.price * i.qty) AS total
  FROM orders o JOIN order_items i ON i.order_id = o.id JOIN products p ON p.id = i.product_id
  GROUP BY o.id, o.customer_id;

INSERT INTO categories(name) VALUES ('Books'),('Games');
INSERT INTO categories(name,parent_id) VALUES ('Sci-Fi',1);
INSERT INTO customers(email,full_name,profile) VALUES ('ann@example.com','Ann Lee','{"vip":true}'),('bob@example.com','Bob Ito',NULL);
INSERT INTO products(sku,name,price,category_id) VALUES ('BK000001','Dune',9.99,3),('GM000001','Chess',19.50,2);
INSERT INTO orders(customer_id,status,ordered_on,total) VALUES (1,'paid','2026-01-02',29.49),(2,'new','2026-01-03',NULL);
INSERT INTO order_items VALUES (1,1,1,1),(1,2,2,1),(2,1,1,3);
INSERT INTO shipments(order_id,line_no,shipped) VALUES (1,1,1);

CREATE DATABASE IF NOT EXISTS analytics;
CREATE TABLE analytics.page_views (id BIGINT AUTO_INCREMENT PRIMARY KEY, path VARCHAR(500), customer_id BIGINT,
  CONSTRAINT fk_pv_cust FOREIGN KEY (customer_id) REFERENCES shop.customers(id));
GRANT ALL ON analytics.* TO 'dbc'@'%';
GRANT REPLICATION CLIENT, PROCESS ON *.* TO 'dbc'@'%';
