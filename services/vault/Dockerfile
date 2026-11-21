FROM openbao/openbao:2.2.0
USER root
RUN apk add --no-cache curl jq
COPY config.hcl /openbao/config/dl-vault.hcl
COPY scripts/entrypoint.sh scripts/backup.sh /usr/local/bin/
RUN chmod 0755 /usr/local/bin/entrypoint.sh /usr/local/bin/backup.sh
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
