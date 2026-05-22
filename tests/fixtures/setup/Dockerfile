# Builds an nginx image with vozlt/nginx-module-vts compiled in.
# Used solely to (re)generate VTS JSON fixtures under tests/fixtures/.
# Not shipped as part of the vozltop binary.
ARG NGINX_VERSION=1.27.3
ARG NGINX_SHA256=ba23a9568f442036b61cd0e29bd66a47b90634efa91e0b2cf2d719057a9b7903
ARG VTS_VERSION=0.2.5
ARG VTS_SHA256=cac326607e6166d9e4b03e3d3b29e520ed43bc86516aff1a946f7d326ca9e19b

FROM nginx:${NGINX_VERSION}-alpine AS build
ARG NGINX_VERSION
ARG NGINX_SHA256
ARG VTS_VERSION
ARG VTS_SHA256

# Pinned tarballs are sha256-verified so that "なぜ JSON が変わったか" の
# 調査時に上流差し替え可能性を最初に排除できる。バージョンを上げる際は
# NGINX_SHA256 / VTS_SHA256 も同時に更新すること。
RUN apk add --no-cache --virtual .build \
        gcc make musl-dev pcre-dev zlib-dev openssl-dev linux-headers curl \
    && cd /tmp \
    && curl -fsSL -o nginx.tar.gz https://nginx.org/download/nginx-${NGINX_VERSION}.tar.gz \
    && echo "${NGINX_SHA256}  nginx.tar.gz" | sha256sum -c - \
    && curl -fsSL -o vts.tar.gz https://github.com/vozlt/nginx-module-vts/archive/refs/tags/v${VTS_VERSION}.tar.gz \
    && echo "${VTS_SHA256}  vts.tar.gz" | sha256sum -c - \
    && tar xzf nginx.tar.gz \
    && tar xzf vts.tar.gz \
    && cd nginx-${NGINX_VERSION} \
    && ./configure \
        --with-compat \
        --add-dynamic-module=/tmp/nginx-module-vts-${VTS_VERSION} \
    && make modules \
    && cp objs/ngx_http_vhost_traffic_status_module.so /tmp/ \
    && apk del .build

FROM nginx:${NGINX_VERSION}-alpine
ARG NGINX_VERSION

COPY --from=build /tmp/ngx_http_vhost_traffic_status_module.so /etc/nginx/modules/

RUN mkdir -p /var/cache/nginx/cache && chown -R nginx:nginx /var/cache/nginx
