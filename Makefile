.PHONY: image run run-headless clean

image:
	cargo build

run:
	cargo run

run-headless:
	cargo run -- --headless

clean:
	cargo clean
	rm -rf build

