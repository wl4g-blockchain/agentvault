ARG NODE_IMAGE=registry.cn-shenzhen.aliyuncs.com/wl4g/node:22-alpine
ARG WEB_IMAGE=registry.cn-shenzhen.aliyuncs.com/wl4g/nginxinc_nginx-unprivileged:1.27-alpine

FROM ${NODE_IMAGE} AS builder
WORKDIR /workspace
COPY package.json package-lock.json ./
RUN npm ci
COPY . .
RUN npm run build

FROM ${WEB_IMAGE}
COPY nginx.conf /etc/nginx/conf.d/default.conf
COPY --from=builder /workspace/dist /usr/share/nginx/html
EXPOSE 8080
