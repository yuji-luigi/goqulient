-- Test replica: follow mysql-primary using GTID auto-positioning, then become read-only.
CHANGE REPLICATION SOURCE TO SOURCE_HOST='mysql-primary', SOURCE_USER='repl', SOURCE_PASSWORD='replpass',
  SOURCE_AUTO_POSITION=1, GET_SOURCE_PUBLIC_KEY=1, SOURCE_CONNECT_RETRY=5;
START REPLICA;
SET PERSIST read_only = ON;
SET PERSIST super_read_only = ON;
