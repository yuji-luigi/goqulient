#!/bin/bash
set -e
if [ ! -f /keys/id_ed25519 ]; then ssh-keygen -q -t ed25519 -N '' -f /keys/id_ed25519 -C dbclient-test; fi
if [ ! -f /keys/id_rsa ]; then ssh-keygen -q -t rsa -b 3072 -N 'keypass' -f /keys/id_rsa -C dbclient-test-rsa; fi
chmod 644 /keys/id_ed25519 /keys/id_rsa
mkdir -p /home/dbc/.ssh && cat /keys/id_ed25519.pub /keys/id_rsa.pub > /home/dbc/.ssh/authorized_keys
chown -R dbc:dbc /home/dbc/.ssh && chmod 700 /home/dbc/.ssh && chmod 600 /home/dbc/.ssh/authorized_keys
cp /etc/ssh/ssh_host_ed25519_key.pub /keys/host_ed25519.pub
exec /usr/sbin/sshd -D -e
