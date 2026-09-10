FROM node:20-alpine

WORKDIR /app

COPY package.json .
RUN npm install

COPY . .

ARG VITE_API_URL=http://localhost:8000
ENV VITE_API_URL=$VITE_API_URL

RUN npm run build

# Use nginx to serve the built files
FROM nginx:alpine

# openssl is not in nginx:alpine, and the entrypoint needs it to generate a
# self-signed certificate when none is mounted.
RUN apk add --no-cache openssl

COPY --from=0 /app/dist /usr/share/nginx/html

# The location blocks, shared by the HTTP and HTTPS server blocks that
# docker-entrypoint.d/05-tls.sh writes at startup.
COPY app-locations.conf /etc/nginx/snippets/app-locations.conf
COPY docker-entrypoint.d/05-tls.sh /docker-entrypoint.d/05-tls.sh
RUN chmod +x /docker-entrypoint.d/05-tls.sh

EXPOSE 80 443

CMD ["nginx", "-g", "daemon off;"]
