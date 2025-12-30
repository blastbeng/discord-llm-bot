import os
import sys
import logging
from sqlalchemy import create_engine, insert, select, update, delete, Table, Column, Integer, String, MetaData, func

SQLITE          = 'sqlite'
SENTENCES     = 'sentences'

class Database:
  DB_ENGINE = {
      SQLITE: 'sqlite:///config/{DB}'
  }

  # Main DB Connection Ref Obj
  db_engine = None
  def __init__(self, dbtype, username='', password='', dbname=''):
    dbtype = dbtype.lower()
    engine_url = self.DB_ENGINE[dbtype].format(DB=dbname)
    self.db_engine = create_engine(engine_url)

  metadata = MetaData()

  sentences = Table(SENTENCES, metadata,
                Column('id', Integer, primary_key=True, autoincrement=True),
                Column('sentence', String(50), nullable=False)
                )

def create_db_tables(self):
  try:
    self.metadata.create_all(self.db_engine)
  except Exception as e:
    exc_type, exc_obj, exc_tb = sys.exc_info()
    fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
    logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)

def insert_sentence(self, sentence: str):
  try:
    stmt = insert(self.sentences).values(sentence=sentence).prefix_with('OR IGNORE')
    compiled = stmt.compile()
    with self.db_engine.connect() as conn:
      result = conn.execute(stmt)
      conn.commit()
  except Exception as e:
    exc_type, exc_obj, exc_tb = sys.exc_info()
    fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
    logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)

def select_like_sentence(self, text: str):
  try:
    value = []
    stmt = select(self.sentences.c.sentence).where(self.sentences.c.sentence.like('%'+text+'%')).order_by(func.random())
    compiled = stmt.compile()
    with self.db_engine.connect() as conn:
      cursor = conn.execute(stmt)
      records = cursor.fetchall()

      for row in records:
        value.append(row[0])
        cursor.close()
      return value
  except Exception as e:
    exc_type, exc_obj, exc_tb = sys.exc_info()
    fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
    logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
    return value

def select_all_sentence(self):
  try:
    value = []
    stmt = select(self.sentences.c.sentence).order_by(func.random())
    compiled = stmt.compile()
    with self.db_engine.connect() as conn:
      cursor = conn.execute(stmt)
      records = cursor.fetchall()

      for row in records:
        value.append(row[0])
        cursor.close()
      return value
  except Exception as e:
    exc_type, exc_obj, exc_tb = sys.exc_info()
    fname = os.path.split(exc_tb.tb_frame.f_code.co_filename)[1]
    logging.error("%s %s %s", exc_type, fname, exc_tb.tb_lineno, exc_info=1)
    return value
