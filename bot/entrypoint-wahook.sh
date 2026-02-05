#!/usr/bin/env bash
gunicorn wahook-bot:app -k eventlet -c config_gunicorn.py --preload --capture-output --enable-stdio-inheritance
