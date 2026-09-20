#include <libeis.h>
#include <poll.h>
#include <stdio.h>
#include <signal.h>
#include <stdlib.h>
#include <string.h>
#include <sys/un.h>
#include <unistd.h>
#include <errno.h>
static volatile sig_atomic_t stop_flag = 0;
struct state {
    struct eis_client *client;
    struct eis_seat *seat;
    struct eis_device *abs;
    struct eis_device *kbd;
};
static struct state st;
static void add_abs(void) {
    if (st.abs) return;
    st.abs = eis_seat_new_device(st.seat);
    eis_device_configure_name(st.abs, "smoke abs");
    eis_device_configure_capability(st.abs, EIS_DEVICE_CAP_POINTER_ABSOLUTE);
    eis_device_configure_capability(st.abs, EIS_DEVICE_CAP_BUTTON);
    eis_device_configure_capability(st.abs, EIS_DEVICE_CAP_SCROLL);
    struct eis_region *region = eis_device_new_region(st.abs);
    eis_region_set_size(region, 800, 600);
    eis_region_set_offset(region, 100, 200);
    eis_region_set_mapping_id(region, "demo");
    eis_region_add(region);
    eis_device_add(st.abs);
    eis_device_resume(st.abs);
}
static void add_kbd(void) {
    if (st.kbd) return;
    st.kbd = eis_seat_new_device(st.seat);
    eis_device_configure_name(st.kbd, "smoke keyboard");
    eis_device_configure_capability(st.kbd, EIS_DEVICE_CAP_KEYBOARD);
    eis_device_add(st.kbd);
    eis_device_resume(st.kbd);
}
static void handle(struct eis_event *event) {
    switch (eis_event_get_type(event)) {
    case EIS_EVENT_CLIENT_CONNECT:
        st.client = eis_event_get_client(event);
        eis_client_connect(st.client);
        st.seat = eis_client_new_seat(st.client, "default");
        eis_seat_configure_capability(st.seat, EIS_DEVICE_CAP_POINTER_ABSOLUTE);
        eis_seat_configure_capability(st.seat, EIS_DEVICE_CAP_BUTTON);
        eis_seat_configure_capability(st.seat, EIS_DEVICE_CAP_SCROLL);
        eis_seat_configure_capability(st.seat, EIS_DEVICE_CAP_KEYBOARD);
        eis_seat_add(st.seat);
        break;
    case EIS_EVENT_SEAT_BIND:
        st.seat = eis_event_get_seat(event);
        if (eis_event_seat_has_capability(event, EIS_DEVICE_CAP_POINTER_ABSOLUTE)) add_abs();
        if (eis_event_seat_has_capability(event, EIS_DEVICE_CAP_KEYBOARD)) add_kbd();
        break;
    case EIS_EVENT_POINTER_MOTION_ABSOLUTE:
        printf("absolute %.2f %.2f\n", eis_event_pointer_get_absolute_x(event), eis_event_pointer_get_absolute_y(event)); fflush(stdout); break;
    case EIS_EVENT_BUTTON_BUTTON:
        printf("button %u %d\n", eis_event_button_get_button(event), eis_event_button_get_is_press(event)); fflush(stdout); break;
    case EIS_EVENT_SCROLL_DELTA:
        printf("scroll %.2f %.2f\n", eis_event_scroll_get_dx(event), eis_event_scroll_get_dy(event)); fflush(stdout); break;
    case EIS_EVENT_KEYBOARD_KEY:
        printf("key %u %d\n", eis_event_keyboard_get_key(event), eis_event_keyboard_get_key_is_press(event)); fflush(stdout); break;
    case EIS_EVENT_CLIENT_DISCONNECT:
        stop_flag = 1; break;
    default: break;
    }
}
int main(int argc, char **argv) {
    const char *path = argc > 1 ? argv[1] : "/tmp/dcu-eis-real.sock";
    unlink(path);
    struct eis *eis = eis_new(NULL);
    if (!eis || eis_setup_backend_socket(eis, path) != 0) return 2;
    struct pollfd pfd = {eis_get_fd(eis), POLLIN, 0};
    while (!stop_flag) {
        int rc = poll(&pfd, 1, 1000);
        if (rc < 0) { if (errno == EINTR) continue; return 3; }
        if (rc == 0) continue;
        eis_dispatch(eis);
        struct eis_event *event;
        while ((event = eis_get_event(eis))) {
            handle(event);
            eis_event_unref(event);
        }
    }
    eis_unref(eis);
    unlink(path);
    return 0;
}



