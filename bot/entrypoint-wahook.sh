#!/usr/bin/env bash
export PYTHONUNBUFFERED=1
gunicorn wahook-bot:app --worker-class gthread -k gthread -c config_gunicorn.py --capture-output --enable-stdio-inheritance
