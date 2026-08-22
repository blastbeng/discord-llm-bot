FROM golang:1.24-alpine AS builder
RUN apk add --no-cache gcc musl-dev ffmpeg git
WORKDIR /app
ENV GOPROXY=https://proxy.golang.org,direct
COPY go.* ./
RUN go mod download
COPY . .
RUN CGO_ENABLED=1 go build -o /discord-bot .

FROM alpine:latest
RUN apk add --no-cache ffmpeg
WORKDIR /app
COPY --from=builder /discord-bot .
CMD ["./discord-bot"]
