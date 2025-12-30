#/bin/sh
python3 -m venv .venv
source .venv/bin/activate; pip3 install -r requirements.txt
git clone https://github.com/Rapptz/discord.py discord.py
cd ./discord.py
pip3 install -U .[voice]
cd ..
rm -rf discord.py 