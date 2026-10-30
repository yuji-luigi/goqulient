# Throwaway SSH bastion for the tunnel tests (adapted from the Goku app's docker/ssh). Password: dbc / sshpass. Test key
# pairs and copies of the host keys are written at first start into the `ssh-keys` volume, which only the `engine`
# container mounts (read-only); they never leave Docker.
FROM alpine:3.20
RUN apk add --no-cache openssh bash \
 && ssh-keygen -A \
 && adduser -D -s /bin/bash dbc && echo 'dbc:sshpass' | chpasswd \
 && sed -i -E 's/^#?AllowTcpForwarding.*/AllowTcpForwarding yes/; s/^#?PasswordAuthentication.*/PasswordAuthentication yes/' /etc/ssh/sshd_config
COPY entrypoint.sh /entrypoint.sh
ENTRYPOINT ["/bin/bash", "/entrypoint.sh"]
