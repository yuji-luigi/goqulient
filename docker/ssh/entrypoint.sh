#!/bin/bash
# Test keys (the passphrase of the encrypted ones is `keypass`): id_ed25519 (none), id_ed25519_enc and id_rsa (encrypted).
set -e
key() { [ -f "/keys/$1" ] || ssh-keygen -q -t "$2" ${3:+-b $3} -N "$4" -f "/keys/$1" -C "goqulient-test-$1"; }
key id_ed25519 ed25519 '' ''
key id_ed25519_enc ed25519 '' keypass
key id_rsa rsa 3072 keypass
chmod 644 /keys/id_*
mkdir -p /home/dbc/.ssh && cat /keys/id_ed25519.pub /keys/id_ed25519_enc.pub /keys/id_rsa.pub > /home/dbc/.ssh/authorized_keys
chown -R dbc:dbc /home/dbc/.ssh && chmod 700 /home/dbc/.ssh && chmod 600 /home/dbc/.ssh/authorized_keys
for t in ed25519 ecdsa rsa; do cp "/etc/ssh/ssh_host_${t}_key.pub" "/keys/host_${t}.pub"; done
exec /usr/sbin/sshd -D -e
