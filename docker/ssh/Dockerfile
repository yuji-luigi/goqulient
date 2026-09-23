# Throwaway SSH bastion for tunnel tests. Password: dbc/sshpass. A key pair is generated at
# first start into the shared /keys volume (test-only, never leaves Docker).
FROM alpine:3.20
RUN apk add --no-cache openssh bash \
 && ssh-keygen -A \
 && adduser -D -s /bin/bash dbc && echo 'dbc:sshpass' | chpasswd \
 && sed -i -E 's/^#?AllowTcpForwarding.*/AllowTcpForwarding yes/; s/^#?PasswordAuthentication.*/PasswordAuthentication yes/' /etc/ssh/sshd_config
COPY entrypoint.sh /entrypoint.sh
ENTRYPOINT ["/bin/bash", "/entrypoint.sh"]
