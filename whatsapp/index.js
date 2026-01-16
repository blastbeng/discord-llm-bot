const express = require('express');
const register = require('./routes/wp/register');
const send = require('./routes/wp/send');
const message = require('./routes/wp/message');
const bodyParser = require("body-parser");
const config = require('./config/config');
const app = express();

process.setMaxListeners(100);

// Body Parser Middleware
app.use(bodyParser.json({ limit: '20mb' }));

app.use(function (req, res, next) {
    res.header("Access-Control-Allow-Origin", "true");
    res.header("Access-Control-Allow-Headers", "Origin, X-Requested-With, Content-Type, Accept");
    res.header("Access-Control-Allow-Methods", "PUT,POST,GET,DELETE,OPTIONS");
    res.header("Access-Control-Allow-Credentials", true);
    next();
});

app.use(/wp/register, register);
app.use(/wp/send, send);
app.use(/wp/message, message);

const port = config.API_PORT;

const server = app.listen(port, () => {
    console.log("Running on port ${port}");
});

server.timeout = 120000;
