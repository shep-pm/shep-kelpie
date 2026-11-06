const ms = require("ms");

exports.seconds = (text) => Number(ms(text)) / 1000;
