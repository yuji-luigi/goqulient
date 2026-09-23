#!/bin/bash
# Initialises the MongoDB replica set (rs0) and, if present, the sharded cluster. Idempotent.
set -e
wait_for() { until mongosh --quiet --host "$1" --eval 'db.adminCommand({ping:1}).ok' >/dev/null 2>&1; do sleep 1; done; }

wait_for mongo-rs1:27017
mongosh --quiet --host mongo-rs1:27017 --eval '
try { rs.status() } catch (e) {
  rs.initiate({ _id: "rs0", members: [
    { _id: 0, host: "mongo-rs1:27017", priority: 2 },
    { _id: 1, host: "mongo-rs2:27017" },
    { _id: 2, host: "mongo-rs3:27017" } ] })
}'
until mongosh --quiet --host mongo-rs1:27017 --eval 'db.hello().isWritablePrimary' | grep -q true; do sleep 1; done
mongosh --quiet --host mongo-rs1:27017 --eval '
const d = db.getSiblingDB("rsdb");
if (d.items.countDocuments() === 0) { d.owners.insertOne({ _id: 1, name: "o" }); d.items.insertMany([{ ownerId: 1, n: 1 }, { ownerId: 1, n: 2 }], { writeConcern: { w: "majority" } }) }'
echo "rs0 ready"

if getent hosts mongos >/dev/null; then
  wait_for mongo-cfg:27019
  mongosh --quiet --host mongo-cfg:27019 --eval 'try { rs.status() } catch (e) { rs.initiate({ _id: "cfg", configsvr: true, members: [{ _id: 0, host: "mongo-cfg:27019" }] }) }'
  wait_for mongo-shard1:27018
  mongosh --quiet --host mongo-shard1:27018 --eval 'try { rs.status() } catch (e) { rs.initiate({ _id: "sh1", members: [{ _id: 0, host: "mongo-shard1:27018" }] }) }'
  until mongosh --quiet --host mongo-shard1:27018 --eval 'db.hello().isWritablePrimary' | grep -q true; do sleep 1; done
  wait_for mongos:27017
  mongosh --quiet --host mongos:27017 --eval '
  if (!db.adminCommand({ listShards: 1 }).shards.length) sh.addShard("sh1/mongo-shard1:27018");
  sh.enableSharding("sharded"); sh.shardCollection("sharded.events", { _id: "hashed" });
  const ev = db.getSiblingDB("sharded").events;
  if (ev.countDocuments() === 0) ev.insertMany([{ a: 1 }, { a: 2 }]);'
  echo "sharded cluster ready"
fi
