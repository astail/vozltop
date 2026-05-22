# Builds an nginx image with vozlt/nginx-module-vts compiled in.
# Used solely to (re)generate VTS JSON fixtures under tests/fixtures/.
# Not shipped as part of the vozltop binary.
ARG NGINX_VERSION=1.27.3
ARG VTS_VERSION=0.2.5

FROM nginx:${NGINX_VERSION}-alpine AS build
ARG NGINX_VERSION
ARG VTS_VERSION

RUN apk add --no-cache --virtual .build \
        gcc make musl-dev pcre-dev zlib-dev openssl-dev linux-headers curl \
    && cd /tmp \
    && curl -fsSL -o nginx.tar.gz https://nginx.org/download/nginx-${NGINX_VERSION}.tar.gz \
    && curl -fsSL -o vts.tar.gz https://github.com/vozlt/nginx-module-vts/archive/refs/tags/v${VTS_VERSION}.tar.gz \
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
