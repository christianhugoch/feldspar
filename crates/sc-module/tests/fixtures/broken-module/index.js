// Requiring this module throws, which is what a module with a syntax error or a
// missing dependency does. The host reports it and stays up.
throw new Error("this module cannot be loaded");
