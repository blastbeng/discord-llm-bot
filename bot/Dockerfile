FROM golang:1.22-alpine AS builder
RUN apk add --no-cache gcc musl-dev ffmpeg
WORKDIR /app
COPY go.mod go.sum ./
RUN go mod download
COPY . .
RUN CGO_ENABLED=1 go build -o /discord-bot .

FROM alpine:latest
RUN apk add --no-cache ffmpeg
WORKDIR /app
COPY --from=builder /discord-bot .
CMD ["./discord-bot"]
