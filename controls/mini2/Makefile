HOSTCC ?= cc
CFLAGS = -std=c11 -Wall -Wextra -Werror -g -fsanitize=address,undefined -fno-pie -no-pie

.PHONY: check clean
check:
	$(HOSTCC) $(CFLAGS) test_buttons.c -o test-buttons
	./test-buttons
	$(HOSTCC) $(CFLAGS) test_lights.c -o test-lights
	./test-lights
	$(HOSTCC) $(CFLAGS) ../test_mapped_input.c -o test-mapped-input
	./test-mapped-input
	$(MAKE) clean

clean:
	rm -f test-buttons test-lights test-mapped-input
