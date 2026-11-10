#include "internal.h"
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>

fz_context *mupdf_new_base_context(void);
void mupdf_drop_base_context(fz_context *);
fz_buffer *mupdf_pixmap_get_image_data(fz_context *, fz_pixmap *, int, mupdf_error_t **);
void mupdf_drop_error(mupdf_error_t *);

static int registration_failure;
static int encoding_failure;
static int created_buffers;
static int destroyed_buffers;

void __real_fz_register_document_handlers(fz_context *);
void __wrap_fz_register_document_handlers(fz_context *ctx)
{
    if (registration_failure)
        fz_throw(ctx, FZ_ERROR_GENERIC, "injected handler failure");
    __real_fz_register_document_handlers(ctx);
}

void __real_fz_write_pixmap_as_jpeg(fz_context *, fz_output *, fz_pixmap *, int, int);
void __wrap_fz_write_pixmap_as_jpeg(fz_context *ctx, fz_output *out,
                                  fz_pixmap *pixmap, int quality, int invert)
{
    if (encoding_failure)
        fz_throw(ctx, FZ_ERROR_GENERIC, "injected JPEG failure");
    __real_fz_write_pixmap_as_jpeg(ctx, out, pixmap, quality, invert);
}

fz_buffer *__real_fz_new_buffer(fz_context *, size_t);
fz_buffer *__wrap_fz_new_buffer(fz_context *ctx, size_t capacity)
{
    fz_buffer *buffer = __real_fz_new_buffer(ctx, capacity);
    if (buffer)
        ++created_buffers;
    return buffer;
}

void __real_fz_drop_buffer(fz_context *, fz_buffer *);
void __wrap_fz_drop_buffer(fz_context *ctx, fz_buffer *buffer)
{
    if (buffer && buffer->refs == 1)
        ++destroyed_buffers;
    __real_fz_drop_buffer(ctx, buffer);
}

int main(void)
{
    registration_failure = 1;
    assert(mupdf_new_base_context() == NULL);
    registration_failure = 0;
    fz_context *ctx = mupdf_new_base_context();
    assert(ctx != NULL);
    fz_pixmap *pixmap = fz_new_pixmap(ctx, fz_device_rgb(ctx), 8, 8, NULL, 0);
    assert(pixmap != NULL);
    fz_clear_pixmap_with_value(ctx, pixmap, 255);
    const int retained_before_encoding = created_buffers - destroyed_buffers;

    encoding_failure = 1;
    for (int i = 0; i < 32; ++i) {
        mupdf_error_t *error = NULL;
        fz_buffer *buffer = mupdf_pixmap_get_image_data(ctx, pixmap, 5, &error);
        assert(buffer == NULL);
        assert(error != NULL);
        mupdf_drop_error(error);
        assert(created_buffers - destroyed_buffers == retained_before_encoding);
    }
    encoding_failure = 0;
    mupdf_error_t *error = NULL;
    fz_buffer *buffer = mupdf_pixmap_get_image_data(ctx, pixmap, 5, &error);
    assert(buffer != NULL && error == NULL);
    unsigned char *bytes = NULL;
    size_t length = fz_buffer_storage(ctx, buffer, &bytes);
    assert(length > 4 && bytes[0] == 0xff && bytes[1] == 0xd8
           && bytes[length - 2] == 0xff && bytes[length - 1] == 0xd9);
    fz_drop_buffer(ctx, buffer);
    assert(created_buffers - destroyed_buffers == retained_before_encoding);
    fz_drop_pixmap(ctx, pixmap);
    mupdf_drop_base_context(ctx);
    puts("PASS: registration failure, 32 encoding failures without leaked buffers, recovery JPEG");
    return 0;
}
