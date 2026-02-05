import eventlet
eventlet.monkey_patch()
bind = '0.0.0.0:8090'
backlog = 2048

workers = 4
worker_class = 'gthread'
worker_connections = 1000
timeout = 43200
keepalive = 2
spew = False
capture_output = True
threads = 1

daemon = False

errorlog = '-'
accesslog = '-'
