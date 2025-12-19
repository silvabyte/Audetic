import { openapi } from "@elysiajs/openapi";
import { Elysia } from "elysia";
import { registerRoutes } from "./routes";

/**
 * Create the Elysia app with all routes configured.
 * This function is used both by the main server and for type extraction.
 */
export function createApp() {
	const app = new Elysia();

	// CORS origin from environment variable (defaults to localhost:5173 for dev)
	const allowedOrigin = Bun.env.CORS_ORIGIN ?? "http://localhost:5173";

	// Enable CORS for UI development
	app.use((app) =>
		app.onBeforeHandle(({ set }) => {
			set.headers["Access-Control-Allow-Origin"] = allowedOrigin;
			set.headers["Access-Control-Allow-Methods"] =
				"GET, POST, PUT, DELETE, OPTIONS";
			set.headers["Access-Control-Allow-Headers"] = "Content-Type";
		}),
	);

	app.options("*", ({ set }) => {
		set.headers["Access-Control-Allow-Origin"] = allowedOrigin;
		set.headers["Access-Control-Allow-Methods"] =
			"GET, POST, PUT, DELETE, OPTIONS";
		set.headers["Access-Control-Allow-Headers"] = "Content-Type";
		return "";
	});

	app.use(openapi());

	registerRoutes(app);

	app.get("/", () => "Hello Robots");

	return app;
}

// Export type for Eden Treaty
export type App = ReturnType<typeof createApp>;
