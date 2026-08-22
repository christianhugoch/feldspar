// Stands in for `async-mqtt`: an ordinary npm dependency a module requires.
module.exports = { shout: (what) => `${String(what).toUpperCase()}!` };
